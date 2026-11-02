mod markdown;

use anyhow::{anyhow, Context, Result};
use arboard::Clipboard;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use crossterm::{
    event::{
        DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::{SinkExt, StreamExt};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame, Terminal,
};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::StatefulImage;
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const ACCENT_BLUE: Color = Color::Rgb(20, 120, 255);
const ACCENT_CYAN: Color = Color::Rgb(80, 170, 255);
const CHAT_BG_USER: Color = Color::Rgb(18, 32, 55);

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, Copy, PartialEq)]
enum ThinkingMode {
    Low,
    Medium,
    High,
}

impl ThinkingMode {
    fn next(self) -> Self {
        match self {
            Self::Low => Self::Medium,
            Self::Medium => Self::High,
            Self::High => Self::Low,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }
}

#[derive(Debug, Deserialize)]
struct Config {
    chrome_binary: String,
    profile_dir: String,
    cdp_port: u16,
    headless: bool,
}

#[derive(Debug, Deserialize)]
struct CdpTarget {
    #[serde(rename = "webSocketDebuggerUrl")]
    ws_url: Option<String>,
    #[serde(rename = "type")]
    target_type: String,
    url: String,
}

#[derive(Debug, Clone)]
struct AccountInfo {
    name: String,
    email: String,
}

struct ChromeBridge {
    _process: Child,
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    request_id: AtomicU64,
}

impl ChromeBridge {
    async fn start(cfg: &Config) -> Result<(Self, AccountInfo)> {
        let abs_profile = std::env::current_dir()?.join(&cfg.profile_dir);
        fs::create_dir_all(&abs_profile)?;

        let mut cmd = Command::new(&cfg.chrome_binary);
        cmd.arg(format!("--remote-debugging-port={}", cfg.cdp_port))
            .arg(format!("--user-data-dir={}", abs_profile.to_string_lossy()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-background-timer-throttling")
            .arg("--disable-backgrounding-occluded-windows")
            .arg("--disable-renderer-backgrounding")
            .arg("--disable-features=CalculateNativeWinOcclusion")
            .arg("https://gemini.google.com/app");

        if cfg.headless {
            cmd.arg("--headless=new");
        }

        let child = cmd
            .spawn()
            .context("Не удалось запустить Chrome. Проверьте chrome_binary в config.json")?;

        let endpoint = format!("http://127.0.0.1:{}/json", cfg.cdp_port);
        let mut ws_url = None;

        print!("Инициализация Chrome...");
        io::stdout().flush()?;

        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(1000)).await;
            print!(".");
            io::stdout().flush()?;

            if let Ok(resp) = reqwest::get(&endpoint).await {
                if let Ok(targets) = resp.json::<Vec<CdpTarget>>().await {
                    if let Some(target) = targets
                        .into_iter()
                        .find(|t| t.target_type == "page" && t.url.contains("gemini.google.com"))
                    {
                        if let Some(url) = target.ws_url {
                            ws_url = Some(url);
                            break;
                        }
                    }
                }
            }
        }
        println!();

        let ws_url = ws_url.ok_or_else(|| anyhow!("Не удалось найти открытую вкладку Gemini."))?;
        let (ws, _) = connect_async(&ws_url)
            .await
            .context("Сбой WebSocket CDP подключения")?;

        let mut bridge = Self {
            _process: child,
            ws,
            request_id: AtomicU64::new(1),
        };

        bridge.send_command("Runtime.enable", json!({})).await?;
        bridge.send_command("Page.enable", json!({})).await?;
        bridge.send_command("DOM.enable", json!({})).await?;

        let _ = bridge
            .send_command("Emulation.setFocusEmulationEnabled", json!({ "enabled": true }))
            .await;
        let _ = bridge
            .send_command(
                "Emulation.setPageVisibilityState",
                json!({ "visibilityState": "visible" }),
            )
            .await;

        let account = bridge.ensure_authenticated().await?;
        let _ = bridge.minimize_window().await;

        Ok((bridge, account))
    }

    async fn send_command(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.request_id.fetch_add(1, Ordering::SeqCst);
        let payload = json!({ "id": id, "method": method, "params": params });

        self.ws
            .send(tokio_tungstenite::tungstenite::Message::Text(payload.to_string()))
            .await?;

        while let Some(msg) = self.ws.next().await {
            let msg = msg?;
            if let tokio_tungstenite::tungstenite::Message::Text(txt) = msg {
                let parsed: Value = serde_json::from_str(&txt)?;
                if parsed.get("id").and_then(|v| v.as_u64()) == Some(id) {
                    if let Some(err) = parsed.get("error") {
                        return Err(anyhow!("CDP ошибка: {:?}", err));
                    }
                    return Ok(parsed.get("result").cloned().unwrap_or(Value::Null));
                }
            }
        }
        Err(anyhow!("Соединение закрыто до ответа на команду {}", id))
    }

    async fn evaluate(&mut self, script: &str) -> Result<Value> {
        let res = self
            .send_command(
                "Runtime.evaluate",
                json!({ "expression": script, "returnByValue": true, "awaitPromise": true }),
            )
            .await?;
        Ok(res.get("result").and_then(|r| r.get("value")).cloned().unwrap_or(Value::Null))
    }

    async fn minimize_window(&mut self) -> Result<()> {
        let win_info = self.send_command("Browser.getWindowForTarget", json!({})).await?;
        if let Some(window_id) = win_info.get("windowId").and_then(|v| v.as_i64()) {
            let _ = self
                .send_command(
                    "Browser.setWindowBounds",
                    json!({ "windowId": window_id, "bounds": { "windowState": "minimized" } }),
                )
                .await;
        }
        Ok(())
    }

    async fn get_account_info(&mut self) -> Result<Option<AccountInfo>> {
        let script = r##"
            (() => {
                const avatar = document.querySelector('a[href*="myaccount.google.com"]')
                            || document.querySelector('a[aria-label*="Google Account"]')
                            || document.querySelector('a[aria-label*="Аккаунт Google"]')
                            || document.querySelector('button[aria-label*="Google Account"]')
                            || document.querySelector('button[aria-label*="Аккаунт Google"]');

                if (!avatar) return null;

                const label = avatar.getAttribute('aria-label') || avatar.getAttribute('title') || "";

                let name = "Developer";
                const nameMatch = label.match(/(?:Аккаунт Google|Google Account)[:\s]+([^(\n\r]+)/i);
                if (nameMatch && nameMatch[1]) {
                    const clean = nameMatch[1].trim();
                    if (clean.length > 0 && !clean.toLowerCase().includes("apps")) {
                        name = clean;
                    }
                }

                let email = "";
                const emailMatch = label.match(/([a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,})/);
                if (emailMatch && emailMatch[1]) {
                    email = emailMatch[1].trim();
                }

                return { name, email };
            })()
        "##;

        let res = self.evaluate(script).await?;
        if res.is_null() {
            return Ok(None);
        }

        let name = res.get("name").and_then(|v| v.as_str()).unwrap_or("Developer").to_string();
        let email = res.get("email").and_then(|v| v.as_str()).unwrap_or("").to_string();

        Ok(Some(AccountInfo { name, email }))
    }

    async fn ensure_authenticated(&mut self) -> Result<AccountInfo> {
        let mut account = self.get_account_info().await?;

        if account.is_none() {
            println!("\x1b[1;33m[Внимание] Вход в Google-аккаунт не обнаружен!\x1b[0m");
            println!("Авторизуйтесь в открытом окне Chrome...");
            print!("Ожидание завершения авторизации...");
            io::stdout().flush()?;

            while account.is_none() {
                tokio::time::sleep(Duration::from_millis(2000)).await;
                print!(".");
                io::stdout().flush()?;
                account = self.get_account_info().await?;
            }
            println!();
        }

        let info = account.unwrap();
        println!("\x1b[1;32m[OK] Аккаунт: {} ({})\x1b[0m", info.name, info.email);
        Ok(info)
    }

    /// Выбирает уровень рассуждений в меню режима Gemini. Возвращает true, если сайт принял выбор.
    async fn set_thinking_level(&mut self, mode: ThinkingMode) -> Result<bool> {
        let level = match mode {
            ThinkingMode::Low => "0",
            ThinkingMode::Medium => "1",
            ThinkingMode::High => "2",
        };
        let script = SET_THINKING_LEVEL_JS.replace("__LEVEL__", level);
        let res = self.evaluate(&script).await?;
        Ok(res.as_str() == Some("ok"))
    }

    async fn fetch_usage_info(&mut self) -> Result<String> {
        let script = r###"
            (async () => {
                try {
                    const r = await fetch('https://gemini.google.com/usage', {
                        credentials: 'include',
                        headers: { 'Accept': 'text/html,application/xhtml+xml,application/xml' }
                    });
                    if (!r.ok) return null;
                    const html = await r.text();

                    const usedMatches = Array.from(html.matchAll(/(\d{1,3}%\s*used)/gi)).map(m => m[1]);
                    const resetMatches = Array.from(html.matchAll(/Resets\s+(?:at\s+)?([0-9]{1,2}:[0-9]{2}\s*(?:[AP]M)?)/gi)).map(m => m[1]);
                    const weekMatches = Array.from(html.matchAll(/Resets\s+([A-Za-z]{3}\s+\d{1,2}(?:\s+at\s+[0-9]{1,2}:[0-9]{2}\s*(?:[AP]M)?)?)/gi)).map(m => m[1]);

                    let current = "";
                    let weekly = "";

                    if (usedMatches.length > 0) {
                        current = usedMatches[0].trim();
                        if (resetMatches.length > 0) {
                            current += " (Resets " + resetMatches[0].trim() + ")";
                        }
                    }
                    if (usedMatches.length > 1) {
                        weekly = "Week: " + usedMatches[1].trim();
                    } else if (html.includes("Weekly limit")) {
                        const m = html.match(/Weekly limit[\s\S]*?(\d{1,3}%\s*used)/i);
                        if (m) weekly = "Week: " + m[1].trim();
                    }

                    if (current && weekly) {
                        return current + " | " + weekly;
                    } else if (current) {
                        return current;
                    }

                    const anyUsed = html.match(/(\d{1,3}%\s*used)/i);
                    if (anyUsed) return anyUsed[1].trim();

                    return null;
                } catch(e) {
                    return null;
                }
            })()
        "###;

        let res = self.evaluate(script).await?;
        if let Some(txt) = res.as_str() {
            if !txt.is_empty() {
                return Ok(txt.to_string());
            }
        }
        Ok("Limits: 5h cycle active".to_string())
    }

    async fn attach_image(&mut self, file_path: &Path) -> Result<()> {
        let bytes = fs::read(file_path).context("Не удалось прочитать файл изображения")?;
        let base64_data = BASE64.encode(&bytes);
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image.png");

        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png")
            .to_lowercase();

        let mime = match ext.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            "bmp" => "image/bmp",
            "gif" => "image/gif",
            _ => "image/png",
        };

        let script = format!(
            r###"
            (async () => {{
                try {{
                    const b64 = "{b64}";
                    const byteChars = atob(b64);
                    const byteNums = new Array(byteChars.length);
                    for (let i = 0; i < byteChars.length; i++) {{
                        byteNums[i] = byteChars.charCodeAt(i);
                    }}
                    const byteArray = new Uint8Array(byteNums);
                    const blob = new Blob([byteArray], {{ type: "{mime}" }});
                    const file = new File([blob], "{file_name}", {{ type: "{mime}" }});

                    const target = document.querySelector('rich-textarea')
                                || document.querySelector('.ql-editor')
                                || document.querySelector('[contenteditable="true"]')
                                || document.body;

                    target.focus();

                    const dataTransfer = new DataTransfer();
                    dataTransfer.items.add(file);

                    const pasteEvent = new ClipboardEvent('paste', {{
                        bubbles: true,
                        cancelable: true,
                        clipboardData: dataTransfer
                    }});
                    target.dispatchEvent(pasteEvent);

                    const dropEvent = new DragEvent('drop', {{
                        bubbles: true,
                        cancelable: true,
                        dataTransfer: dataTransfer
                    }});
                    target.dispatchEvent(dropEvent);

                    return true;
                }} catch (e) {{
                    return false;
                }}
            }})()
            "###,
            b64 = base64_data,
            mime = mime,
            file_name = file_name
        );

        self.evaluate(&script).await?;

        let wait_script = r##"
            (() => {
                const preview = document.querySelector('image-preview')
                             || document.querySelector('.image-preview')
                             || document.querySelector('img[src^="blob:"]')
                             || document.querySelector('.file-preview')
                             || document.querySelector('.thumbnail-preview')
                             || document.querySelector('.attachment-container')
                             || document.querySelector('uploader-file-item');
                return preview !== null;
            })()
        "##;

        for _ in 0..25 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if self.evaluate(wait_script).await?.as_bool().unwrap_or(false) {
                return Ok(());
            }
        }

        Ok(())
    }

    async fn get_response_count(&mut self) -> Result<usize> {
        let script = r##"
            (() => {
                const responses = document.querySelectorAll('model-response, .model-response, [data-test-id="model-response"]');
                return responses.length;
            })()
        "##;
        let res = self.evaluate(script).await?;
        Ok(res.as_u64().unwrap_or(0) as usize)
    }

    async fn send_message(&mut self, text: &str) -> Result<()> {
        let escaped = serde_json::to_string(text)?;
        let script = format!(
            r##"
            (() => {{
                const inputArea = document.querySelector('rich-textarea p')
                               || document.querySelector('.ql-editor p')
                               || document.querySelector('textarea')
                               || document.querySelector('[contenteditable="true"]');
                if (!inputArea) throw new Error("Поле ввода не найдено");

                inputArea.focus();
                document.execCommand('selectAll', false, null);
                document.execCommand('insertText', false, {escaped});

                inputArea.dispatchEvent(new Event('input', {{ bubbles: true, cancelable: true }}));
                inputArea.dispatchEvent(new Event('change', {{ bubbles: true, cancelable: true }}));

                const sendBtn = document.querySelector('button.send-button')
                             || document.querySelector('button[aria-label*="Send"]')
                             || document.querySelector('button[aria-label*="Отправить"]')
                             || document.querySelector('.send-button-container button');

                if (sendBtn && !sendBtn.disabled && sendBtn.getAttribute('aria-disabled') !== 'true') {{
                    sendBtn.click();
                    return true;
                }}

                const enterDown = new KeyboardEvent('keydown', {{
                    key: 'Enter', code: 'Enter', keyCode: 13, which: 13, bubbles: true, cancelable: true
                }});
                inputArea.dispatchEvent(enterDown);
                return true;
            }})()
            "##,
            escaped = escaped
        );

        self.evaluate(&script).await?;
        Ok(())
    }

    async fn poll_response(&mut self, target_index: usize) -> Result<(String, bool)> {
        let script = format!(
            r###"
            (() => {{
                const responses = document.querySelectorAll('model-response, .model-response, [data-test-id="model-response"]');
                if (responses.length <= {idx}) {{
                    return {{ text: "", isDone: false }};
                }}

                const targetMsg = responses[{idx}];
                const contentEl = targetMsg.querySelector('message-content')
                               || targetMsg.querySelector('.model-response-text')
                               || targetMsg.querySelector('.response-content')
                               || targetMsg;

                function nodeToMarkdown(node) {{
                    if (!node) return "";
                    if (node.nodeType === Node.TEXT_NODE) return node.textContent;
                    if (node.nodeType !== Node.ELEMENT_NODE) return "";

                    const tag = node.tagName.toLowerCase();
                    if (['button', 'mat-icon', 'message-actions', 'mat-progress-bar', 'mat-progress-spinner'].includes(tag)
                        || node.classList.contains('code-block-decoration')
                        || node.classList.contains('citation')
                        || node.getAttribute('aria-hidden') === 'true') {{
                        return "";
                    }}

                    const childrenMd = () => Array.from(node.childNodes).map(nodeToMarkdown).join('');

                    switch (tag) {{
                        case 'p':
                            return childrenMd().trim() + '\n\n';
                        case 'h1':
                            return '# ' + childrenMd().trim() + '\n\n';
                        case 'h2':
                            return '## ' + childrenMd().trim() + '\n\n';
                        case 'h3':
                            return '### ' + childrenMd().trim() + '\n\n';
                        case 'h4':
                        case 'h5':
                        case 'h6':
                            return '#### ' + childrenMd().trim() + '\n\n';
                        case 'strong':
                        case 'b':
                            return '**' + childrenMd() + '**';
                        case 'em':
                        case 'i':
                            return '*' + childrenMd() + '*';
                        case 'code':
                            if (node.closest('pre')) return childrenMd();
                            return '`' + childrenMd() + '`';
                        case 'pre': {{
                            const codeEl = node.querySelector('code');
                            let lang = '';
                            if (codeEl) {{
                                const match = (codeEl.className || '').match(/language-(\w+)/);
                                if (match) lang = match[1];
                            }}
                            const rawCode = codeEl ? codeEl.innerText : node.innerText;
                            return '\n```' + lang + '\n' + rawCode.trim() + '\n```\n\n';
                        }}
                        case 'ul':
                        case 'ol': {{
                            // Вложенные списки и многострочные пункты сдвигаются на 4 пробела,
                            // чтобы Markdown сохранил их структуру.
                            const ordered = tag === 'ol';
                            let out = [];
                            let num = parseInt(node.getAttribute('start') || '1', 10) || 1;
                            for (const li of node.children) {{
                                if (li.tagName.toLowerCase() !== 'li') continue;
                                const marker = ordered ? (num++) + '. ' : '* ';
                                const body = nodeToMarkdown(li).split('\n').filter(l => l.trim() !== '');
                                if (body.length === 0) continue;
                                out.push(marker + body[0].trim());
                                for (const l of body.slice(1)) out.push('    ' + l);
                            }}
                            return '\n' + out.join('\n') + '\n\n';
                        }}
                        case 'li':
                            return childrenMd();
                        case 'table': {{
                            let lines = [];
                            const trs = node.querySelectorAll('tr');
                            trs.forEach((tr, rIdx) => {{
                                const cols = Array.from(tr.querySelectorAll('th, td')).map(td => nodeToMarkdown(td).trim().replace(/\n/g, ' '));
                                lines.push('| ' + cols.join(' | ') + ' |');
                                if (rIdx === 0) {{
                                    lines.push('| ' + cols.map(() => '---').join(' | ') + ' |');
                                }}
                            }});
                            return lines.join('\n') + '\n\n';
                        }}
                        case 'blockquote':
                            return '> ' + childrenMd().trim().replace(/\n/g, '\n> ') + '\n\n';
                        case 'br':
                            return '\n';
                        case 'hr':
                            return '\n---\n\n';
                        default:
                            return childrenMd();
                    }}
                }}

                let text = nodeToMarkdown(contentEl).trim();
                if (!text) {{
                    text = contentEl.innerText || "";
                }}

                const stopBtn = document.querySelector('button[aria-label*="Остановить"]')
                             || document.querySelector('button[aria-label*="Stop"]')
                             || document.querySelector('.stop-button');
                const isStopping = stopBtn !== null && stopBtn.offsetParent !== null;

                const isGenerating = document.querySelector('.loading-indicator') !== null
                                  || document.querySelector('mat-progress-bar') !== null
                                  || targetMsg.querySelector('.sparkle-loading') !== null;

                const parent = targetMsg.parentElement || targetMsg;
                const hasActions = parent.querySelector('.message-actions') !== null
                                || parent.querySelector('message-actions') !== null
                                || parent.querySelector('button[aria-label*="Хороший ответ"]') !== null
                                || parent.querySelector('button[aria-label*="Good response"]') !== null
                                || parent.querySelector('button[aria-label*="Копировать"]') !== null
                                || parent.querySelector('button[aria-label*="Copy"]') !== null;

                const isDone = text.length > 0 && !isStopping && !isGenerating && hasActions;
                return {{ text: text, isDone: isDone }};
            }})()
            "###,
            idx = target_index
        );

        let res = self.evaluate(&script).await?;
        let text = res.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let is_done = res.get("isDone").and_then(|d| d.as_bool()).unwrap_or(false);

        Ok((text, is_done))
    }
}

enum Role {
    User,
    Gemini,
}

struct ChatMessage {
    role: Role,
    raw_content: String,
    displayed_chars: usize,
    image_path: Option<PathBuf>,
}

struct AppState {
    input: String,
    messages: Vec<ChatMessage>,
    pending_image: Option<PathBuf>,
    attached_image_state: Option<StatefulProtocol>,
    is_streaming: bool,
    scroll: u16,
    user_name: String,
    user_email: String,
    cwd: String,
    spinner_idx: usize,
    usage_text: String,
    thinking_mode: ThinkingMode,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cfg_str = fs::read_to_string("config.json")
        .context("Файл config.json не найден рядом с Cargo.toml")?;
    let config: Config = serde_json::from_str(&cfg_str)?;

    let (mut bridge, account) = ChromeBridge::start(&config).await?;

    // Программа стартует с уровнем «Low» — выставляем его и на сайте, чтобы индикатор не расходился с Gemini.
    let _ = bridge.set_thinking_level(ThinkingMode::Low).await;

    let initial_usage = bridge
        .fetch_usage_info()
        .await
        .unwrap_or_else(|_| "Normal (5h cycle)".to_string());

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut picker = Picker::from_fontsize((8, 16));
    picker.set_protocol_type(ratatui_image::picker::ProtocolType::Halfblocks);

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let mut state = AppState {
        input: String::new(),
        messages: Vec::new(),
        pending_image: None,
        attached_image_state: None,
        is_streaming: false,
        scroll: 0,
        user_name: account.name,
        user_email: account.email,
        cwd,
        spinner_idx: 0,
        usage_text: initial_usage,
        thinking_mode: ThinkingMode::Low,
    };

    let mut anim_timer = tokio::time::interval(Duration::from_millis(30));
    let mut stream_poll_timer = tokio::time::interval(Duration::from_millis(120));
    let mut usage_timer = tokio::time::interval(Duration::from_secs(30));
    let mut event_stream = EventStream::new();

    let mut current_target_index = 0;
    let mut last_stream_len = 0;
    let mut stream_stable_ticks = 0;
    let mut needs_redraw = true;

    loop {
        if needs_redraw {
            terminal.draw(|f| ui(f, &mut state))?;
            needs_redraw = false;
        }

        tokio::select! {
            biased;

            Some(Ok(ev)) = event_stream.next() => {
                needs_redraw = true;
                match ev {
                    Event::Paste(pasted_text) => {
                        if pasted_text.len() < 300 && !pasted_text.contains('\n') {
                            if let Some(path) = try_simple_image_path(&pasted_text) {
                                if let Ok(dyn_img) = image::open(&path) {
                                    state.attached_image_state = Some(picker.new_resize_protocol(dyn_img));
                                    state.pending_image = Some(path);
                                    continue;
                                }
                            }
                        }
                        state.input.push_str(&pasted_text);
                    }
                    Event::Key(key) => {
                        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
                            continue;
                        }

                        let is_paste = (matches!(
                            key.code,
                            KeyCode::Char('v') | KeyCode::Char('V') | KeyCode::Char('м') | KeyCode::Char('М') | KeyCode::Char('\u{16}')
                        ) && key.modifiers.contains(KeyModifiers::CONTROL))
                            || (key.code == KeyCode::Insert && key.modifiers.contains(KeyModifiers::SHIFT));

                        if is_paste {
                            if let Ok(mut clipboard) = Clipboard::new() {
                                if let Ok(img_data) = clipboard.get_image() {
                                    let temp_path = std::env::temp_dir().join(format!("gemini_clip_{}.png", std::process::id()));
                                    let img_buffer = image::RgbaImage::from_raw(
                                        img_data.width as u32,
                                        img_data.height as u32,
                                        img_data.bytes.into_owned(),
                                    );
                                    if let Some(buf) = img_buffer {
                                        if buf.save(&temp_path).is_ok() {
                                            let dyn_img = image::DynamicImage::ImageRgba8(buf);
                                            state.attached_image_state = Some(picker.new_resize_protocol(dyn_img));
                                            state.pending_image = Some(temp_path);
                                        }
                                    }
                                } else if let Ok(text) = clipboard.get_text() {
                                    if text.len() < 300 && !text.contains('\n') {
                                        if let Some(path) = try_simple_image_path(&text) {
                                            if let Ok(dyn_img) = image::open(&path) {
                                                state.attached_image_state = Some(picker.new_resize_protocol(dyn_img));
                                                state.pending_image = Some(path);
                                                continue;
                                            }
                                        }
                                    }
                                    state.input.push_str(&text);
                                }
                            }
                            continue;
                        }

                        match (key.code, key.modifiers) {
                            (KeyCode::Char('c'), KeyModifiers::CONTROL) => break,

                            (KeyCode::Tab, _) | (KeyCode::F(2), _) => {
                                // Индикатор меняется, только если сайт действительно принял новый уровень.
                                let next = state.thinking_mode.next();
                                if let Ok(true) = bridge.set_thinking_level(next).await {
                                    state.thinking_mode = next;
                                }
                            }

                            // Перенос строки в сообщении: Shift+Enter / Ctrl+Enter (или Ctrl+J, если терминал их не различает).
                            (KeyCode::Enter, m) if m.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL) => {
                                state.input.push('\n');
                            }
                            (KeyCode::Char('j') | KeyCode::Char('о'), KeyModifiers::CONTROL) => {
                                state.input.push('\n');
                            }

                            (KeyCode::Enter, KeyModifiers::NONE) => {
                                let query = state.input.trim().to_string();
                                if (!query.is_empty() || state.pending_image.is_some()) && !state.is_streaming {
                                    let img_to_send = state.pending_image.take();
                                    state.attached_image_state = None;

                                    if let Some(ref img_path) = img_to_send {
                                        let _ = bridge.attach_image(img_path).await;
                                    }

                                    let prompt = if query.is_empty() {
                                        "Что изображено на этой картинке?".to_string()
                                    } else {
                                        query
                                    };

                                    state.messages.push(ChatMessage {
                                        role: Role::User,
                                        raw_content: prompt.clone(),
                                        displayed_chars: prompt.chars().count(),
                                        image_path: img_to_send,
                                    });

                                    state.messages.push(ChatMessage {
                                        role: Role::Gemini,
                                        raw_content: String::new(),
                                        displayed_chars: 0,
                                        image_path: None,
                                    });

                                    state.input.clear();
                                    state.scroll = 0;
                                    current_target_index = bridge.get_response_count().await.unwrap_or(0);
                                    let _ = bridge.send_message(&prompt).await;
                                    state.is_streaming = true;
                                    last_stream_len = 0;
                                    stream_stable_ticks = 0;
                                }
                            }

                            (KeyCode::Backspace, _) => {
                                state.input.pop();
                            }

                            (KeyCode::Up, _) => {
                                state.scroll = state.scroll.saturating_add(2);
                            }

                            (KeyCode::Down, _) => {
                                state.scroll = state.scroll.saturating_sub(2);
                            }

                            (KeyCode::Char(c), KeyModifiers::NONE) | (KeyCode::Char(c), KeyModifiers::SHIFT) => {
                                state.input.push(c);
                            }

                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            _ = stream_poll_timer.tick(), if state.is_streaming => {
                match bridge.poll_response(current_target_index).await {
                    Ok((full_text, is_done)) => {
                        if !full_text.is_empty() {
                            if let Some(last_msg) = state.messages.last_mut() {
                                if matches!(last_msg.role, Role::Gemini) {
                                    last_msg.raw_content = full_text.clone();
                                    needs_redraw = true;
                                }
                            }
                        }

                        if full_text.len() == last_stream_len && !full_text.is_empty() {
                            stream_stable_ticks += 1;
                        } else {
                            stream_stable_ticks = 0;
                            last_stream_len = full_text.len();
                        }

                        if (is_done && stream_stable_ticks >= 2) || stream_stable_ticks >= 15 {
                            state.is_streaming = false;
                            stream_stable_ticks = 0;
                            last_stream_len = 0;
                            if let Some(last_msg) = state.messages.last_mut() {
                                last_msg.displayed_chars = last_msg.raw_content.chars().count();
                            }
                            needs_redraw = true;
                        }
                    }
                    Err(_) => {}
                }
            }

            _ = anim_timer.tick() => {
                let mut animating = false;
                state.spinner_idx = (state.spinner_idx + 1) % SPINNER_FRAMES.len();

                if let Some(last_msg) = state.messages.last_mut() {
                    if matches!(last_msg.role, Role::Gemini) {
                        let total = last_msg.raw_content.chars().count();
                        if last_msg.displayed_chars < total {
                            let diff = total - last_msg.displayed_chars;
                            let step = if diff > 60 { 8 } else if diff > 20 { 4 } else { 2 };
                            last_msg.displayed_chars = (last_msg.displayed_chars + step).min(total);
                            animating = true;
                            needs_redraw = true;
                        }
                    }
                }

                if state.is_streaming || animating {
                    needs_redraw = true;
                }
            }

            _ = usage_timer.tick() => {
                if !state.is_streaming {
                    if let Ok(info) = bridge.fetch_usage_info().await {
                        if state.usage_text != info {
                            state.usage_text = info;
                            needs_redraw = true;
                        }
                    }
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn is_image_extension(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif")
}

fn try_simple_image_path(text: &str) -> Option<PathBuf> {
    let mut trimmed = text.trim();
    if let Some(stripped) = trimmed.strip_prefix('&') {
        trimmed = stripped.trim();
    }
    trimmed = trimmed.trim_matches(|c| c == '"' || c == '\'' || c == '`');
    let mut cleaned = trimmed.trim();

    if let Some(stripped) = cleaned.strip_prefix("file:///") {
        cleaned = stripped;
    } else if let Some(stripped) = cleaned.strip_prefix("file://") {
        cleaned = stripped;
    }

    let p = PathBuf::from(cleaned);
    if p.is_file() && is_image_extension(&p) {
        Some(p)
    } else {
        None
    }
}

const MAX_INPUT_LINES: usize = 8;
const IMAGE_PREVIEW_WIDTH: u16 = 26;

/// Строки поля ввода с переносом. Ширина берётся на 1 меньше, чтобы курсору всегда хватало места.
fn input_lines(input: &str, inner_width: usize) -> Vec<Line<'static>> {
    let width = inner_width.saturating_sub(1).max(4);
    let mut out = Vec::new();
    for (i, part) in input.split('\n').enumerate() {
        let prefix = if i == 0 {
            Span::styled("> ", Style::default().fg(ACCENT_BLUE).add_modifier(Modifier::BOLD))
        } else {
            Span::raw("  ")
        };
        out.extend(markdown::wrap_line(
            vec![prefix, Span::styled(part.to_string(), Style::default().fg(Color::White))],
            2,
            width,
        ));
    }
    out
}

fn chat_lines(state: &AppState, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    if state.messages.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(ACCENT_BLUE)),
            Span::styled("Ready. Ask a question or paste an image.", Style::default().fg(Color::DarkGray)),
        ]));
        return lines;
    }

    for msg in &state.messages {
        match msg.role {
            Role::User => {
                let mut text = msg.raw_content.clone();
                if let Some(ref path) = msg.image_path {
                    text.push_str(&format!("  [Image: {}]", path.file_name().unwrap_or_default().to_string_lossy()));
                }
                for (idx, part) in text.split('\n').enumerate() {
                    let prefix = if idx == 0 {
                        Span::styled("❯ ", Style::default().fg(ACCENT_CYAN).add_modifier(Modifier::BOLD))
                    } else {
                        Span::raw("  ")
                    };
                    let spans = vec![prefix, Span::styled(part.to_string(), Style::default().fg(Color::White))];
                    // Сообщение пользователя — «плашка» с фоном на всю ширину.
                    for mut line in markdown::wrap_line(spans, 2, width.saturating_sub(1)) {
                        let fill = width.saturating_sub(line.width());
                        line.spans.push(Span::raw(" ".repeat(fill)));
                        for span in line.spans.iter_mut() {
                            span.style = span.style.bg(CHAT_BG_USER);
                        }
                        lines.push(line);
                    }
                }
            }
            Role::Gemini => {
                if msg.raw_content.is_empty() && state.is_streaming {
                    let spinner_char = SPINNER_FRAMES[state.spinner_idx];
                    lines.push(Line::from(vec![
                        Span::styled(format!("{} ", spinner_char), Style::default().fg(ACCENT_CYAN).add_modifier(Modifier::BOLD)),
                        Span::styled(format!("Thinking ({}) ...", state.thinking_mode.as_str()), Style::default().fg(Color::DarkGray)),
                    ]));
                } else {
                    let displayed_text: String = msg.raw_content.chars().take(msg.displayed_chars).collect();
                    let parsed = markdown::render_markdown(&displayed_text, width.saturating_sub(2));
                    for (i, mut line) in parsed.into_iter().enumerate() {
                        let lead = if i == 0 {
                            Span::styled("● ", Style::default().fg(ACCENT_BLUE))
                        } else {
                            Span::raw("  ")
                        };
                        line.spans.insert(0, lead);
                        lines.push(line);
                    }

                    if state.is_streaming && msg.displayed_chars < msg.raw_content.chars().count() {
                        lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(SPINNER_FRAMES[state.spinner_idx], Style::default().fg(ACCENT_BLUE)),
                        ]));
                    }
                }
            }
        }
        lines.push(Line::from(""));
    }
    lines
}

fn ui(f: &mut Frame, state: &mut AppState) {
    let has_image = state.attached_image_state.is_some();

    // Высота поля ввода растёт вместе с текстом (до MAX_INPUT_LINES строк).
    let input_outer_width = f
        .area()
        .width
        .saturating_sub(2)
        .saturating_sub(if has_image { IMAGE_PREVIEW_WIDTH } else { 0 });
    let all_input_lines = input_lines(&state.input, input_outer_width.saturating_sub(2) as usize);
    let visible_input = all_input_lines.len().min(MAX_INPUT_LINES);
    let input_height = visible_input as u16 + 2;
    let bottom_height = if has_image { input_height.max(9) } else { input_height };

    let root_layout = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(12),
            Constraint::Min(8),
            Constraint::Length(bottom_height),
        ])
        .split(f.area());

    // --- Gemini Code CDP Banner ---
    let header_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(ACCENT_BLUE))
        .title(Span::styled(" Gemini Code CDP v1.0.26 ", Style::default().fg(ACCENT_BLUE).add_modifier(Modifier::BOLD)));

    let header_area = root_layout[0];
    f.render_widget(header_block, header_area);

    let inner_header = Layout::default()
        .direction(Direction::Horizontal)
        .margin(1)
        .constraints([Constraint::Percentage(64), Constraint::Percentage(36)])
        .split(header_area);

    let welcome_user = format!("Welcome back {}!", state.user_name);
    let meta_account = if !state.user_email.is_empty() {
        format!("Gemini 3.8 Flash · {}", state.user_email)
    } else {
        "Gemini 3.8 Flash".to_string()
    };

    let left_header_text = vec![
        Line::from(vec![Span::styled(
            format!("{:^44}", welcome_user),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from(vec![Span::styled("   ████   ████   ██   ██   █   ██   █   █", Style::default().fg(ACCENT_BLUE).add_modifier(Modifier::BOLD))]),
        Line::from(vec![Span::styled("   █      █      ███ ███   █   ███  █   █", Style::default().fg(ACCENT_BLUE).add_modifier(Modifier::BOLD))]),
        Line::from(vec![Span::styled("   █ ██   ████   █ █ █ █   █   █ █  █   █", Style::default().fg(ACCENT_CYAN).add_modifier(Modifier::BOLD))]),
        Line::from(vec![Span::styled("   █   █  █      █  █  █   █   █  █ █   █", Style::default().fg(ACCENT_CYAN).add_modifier(Modifier::BOLD))]),
        Line::from(vec![Span::styled("   ████   ████   █     █   █   █   ██   █", Style::default().fg(ACCENT_BLUE).add_modifier(Modifier::BOLD))]),
        Line::from(""),
        Line::from(vec![Span::styled(format!("{:^44}", meta_account), Style::default().fg(Color::DarkGray))]),
        Line::from(vec![Span::styled(format!("{:^44}", state.cwd), Style::default().fg(Color::Rgb(120, 150, 190)))]),
    ];
    f.render_widget(Paragraph::new(left_header_text), inner_header[0]);

    let thinking_style = match state.thinking_mode {
        ThinkingMode::Low => Style::default().fg(Color::Gray),
        ThinkingMode::Medium => Style::default().fg(ACCENT_CYAN).add_modifier(Modifier::BOLD),
        ThinkingMode::High => Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
    };

    let right_header_text = vec![
        Line::from(vec![
            Span::styled("Thinking: ", Style::default().fg(ACCENT_CYAN)),
            Span::styled(format!("[ {} ]", state.thinking_mode.as_str()), thinking_style),
            Span::styled(" (Tab to switch in Chrome)", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled("Account & Usage limits", Style::default().fg(ACCENT_CYAN).add_modifier(Modifier::BOLD))]),
        Line::from(vec![
            Span::styled("● ", Style::default().fg(Color::LightGreen)),
            Span::styled(&state.usage_text, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![Span::styled("Refreshed via gemini.google.com/usage", Style::default().fg(Color::DarkGray))]),
        Line::from(""),
        Line::from(vec![Span::styled("Recent activity", Style::default().fg(ACCENT_CYAN))]),
        Line::from(vec![Span::styled("Session active · WebSocket CDP ready", Style::default().fg(Color::DarkGray))]),
    ];
    let right_widget = Paragraph::new(right_header_text)
        .block(Block::default().borders(Borders::LEFT).border_style(Style::default().fg(Color::Rgb(30, 50, 90))));
    f.render_widget(right_widget, inner_header[1]);

    // --- Chat History ---
    let chat_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(30, 50, 90)))
        .title(Span::styled(" Chat History ", Style::default().fg(Color::DarkGray)));
    let chat_inner = chat_block.inner(root_layout[1]);
    // Строки уже перенесены под ширину, поэтому прокрутка считается точно.
    let formatted_lines = chat_lines(state, chat_inner.width.saturating_sub(1) as usize);

    let max_scroll = formatted_lines.len().saturating_sub(chat_inner.height as usize);
    state.scroll = state.scroll.min(max_scroll.min(u16::MAX as usize) as u16);
    let scroll_y = (max_scroll - state.scroll as usize).min(u16::MAX as usize) as u16;

    let chat_widget = Paragraph::new(formatted_lines)
        .block(chat_block)
        .scroll((scroll_y, 0));
    f.render_widget(chat_widget, root_layout[1]);

    // --- Bottom Input Area ---
    let input_area = if has_image {
        let bottom_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(20), Constraint::Length(IMAGE_PREVIEW_WIDTH)])
            .split(root_layout[2]);

        if let Some(ref mut img_state) = state.attached_image_state {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT_BLUE))
                .title(" Image Preview ");

            let inner_area = block.inner(bottom_chunks[1]);
            f.render_widget(block, bottom_chunks[1]);

            let img_widget = StatefulImage::new(None);
            f.render_stateful_widget(img_widget, inner_area, img_state);
        }
        bottom_chunks[0]
    } else {
        root_layout[2]
    };

    let title = if has_image {
        format!(" Message [Tab: Thinking {}] [Shift+Enter: new line] ", state.thinking_mode.as_str())
    } else {
        format!(
            " Input [Tab: Thinking {}] [Shift+Enter: new line] (Paste image or text path) ",
            state.thinking_mode.as_str()
        )
    };
    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT_BLUE))
        .title(Span::styled(title, Style::default().fg(ACCENT_CYAN)));
    let input_inner = input_block.inner(input_area);

    // Показываем последние строки ввода, чтобы курсор всегда был виден.
    let skip = all_input_lines.len() - visible_input;
    let shown: Vec<Line<'static>> = all_input_lines.into_iter().skip(skip).collect();
    let cursor_x = shown.last().map(|l| l.width()).unwrap_or(0) as u16;
    let cursor_y = shown.len().saturating_sub(1) as u16;

    f.render_widget(Paragraph::new(shown).block(input_block), input_area);
    f.set_cursor_position((
        (input_inner.x + cursor_x).min(input_inner.right().saturating_sub(1)),
        (input_inner.y + cursor_y).min(input_inner.bottom().saturating_sub(1)),
    ));
}

/// Скрипт выбора уровня рассуждений. Опирается на атрибуты разметки, а не на текст,
/// поэтому работает при любом языке интерфейса Gemini. __LEVEL__: 0 — низкий, 1 — средний, 2 — высокий.
const SET_THINKING_LEVEL_JS: &str = r##"
(async () => {
    const LEVEL = __LEVEL__; // 0 = низкий, 1 = средний, 2 = высокий
    const sleep = (ms) => new Promise(r => setTimeout(r, ms));
    const waitFor = async (fn, tries = 20) => {
        for (let i = 0; i < tries; i++) {
            const v = fn();
            if (v) return v;
            await sleep(50);
        }
        return null;
    };
    const picker = document.querySelector('[data-test-id="bard-mode-menu-button"]');
    if (!picker) return 'no-picker';
    const isOpen = () => picker.getAttribute('aria-expanded') === 'true';
    const closeMenu = async () => {
        if (!isOpen()) return;
        const backdrop = document.querySelector('.cdk-overlay-backdrop');
        if (backdrop) backdrop.click();
        else picker.click();
        await waitFor(() => !isOpen(), 10);
        if (isOpen()) {
            document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', code: 'Escape', keyCode: 27, bubbles: true }));
        }
    };
    // Строка «Уровень рассуждений» помечена value="thinking_level" — это не зависит от языка интерфейса.
    const findRow = () => {
        const row = document.querySelector('[role="menuitem"][value="thinking_level"]');
        return row && row.offsetParent !== null ? row : null;
    };
    try {
        if (!isOpen()) picker.click();
        const row = await waitFor(findRow);
        if (!row) { await closeMenu(); return 'no-thinking-row'; }

        // Подменю уровней: на него ссылается aria-controls строки. Порядок пунктов: низкий, средний, высокий.
        const getOptions = () => {
            const id = row.getAttribute('aria-controls');
            const sub = id && document.getElementById(id);
            const items = sub ? Array.from(sub.querySelectorAll('[role="menuitem"]')) : [];
            return items.length >= 3 ? items : null;
        };
        if (row.getAttribute('aria-expanded') !== 'true') row.click();
        const options = await waitFor(getOptions);
        if (!options) { await closeMenu(); return 'no-options'; }

        const target = options[LEVEL];
        if (!target.classList.contains('selected')) {
            target.click();
            await waitFor(() => target.classList.contains('selected') || !target.isConnected);
        }
        const ok = target.classList.contains('selected') || !target.isConnected;
        await closeMenu();
        return ok ? 'ok' : 'not-selected';
    } catch (e) {
        await closeMenu();
        return 'error: ' + e.message;
    }
})()
"##;
