//! Google Search AI Mode adapter. Selectors can be updated without touching CDP/TUI.
use crate::browser::{Browser, Config};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{Duration, Instant};

pub const START_URL: &str = "https://www.google.com/search?udm=50";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PageConfig {
    pub input_selector: String,
    pub response_selector: String,
    pub stop_selector: String,
    pub complete_selector: String,
}
impl Default for PageConfig {
    fn default() -> Self {
        Self {
            input_selector: "textarea[name=\"q\"], textarea[placeholder], [contenteditable=\"true\"][role=\"textbox\"]".into(),
            response_selector: "[data-container-id=\"main-col\"] .n6owBd, [data-container-id=\"main-col\"] pre, [data-container-id=\"main-col\"] [role=\"heading\"], [data-testid=\"ai-response\"]".into(),
            stop_selector: "button[aria-label*=\"Stop\"], [role=\"button\"][aria-label*=\"Stop\"], button[aria-label*=\"Остановить\"], [role=\"button\"][aria-label*=\"Остановить\"]".into(),
            complete_selector: "button[aria-label*=\"Good response\"], [role=\"button\"][aria-label*=\"Good response\"], [aria-label*=\"Helpful\"], [aria-label*=\"Хороший ответ\"], [aria-label*=\"Copy\"], [aria-label*=\"Копировать\"]".into(),
        }
    }
}

pub fn is_search_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|url| {
        url.scheme() == "https"
            && matches!(url.host_str(), Some("www.google.com" | "google.com"))
            && url.path() == "/search"
    })
}

#[derive(Debug, Deserialize, Default)]
pub struct Snapshot {
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub busy: bool,
    #[serde(default)]
    pub generating: bool,
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub response_count: usize,
}

/// Stability uses wall time and full text, never just string length or poll count.
pub struct Turn {
    baseline: String,
    count: usize,
    last_text: String,
    started: Instant,
    changed: Instant,
    timeout: Duration,
    saw_busy: bool,
}
impl Turn {
    pub fn is_new_response(&self,snapshot:&Snapshot)->bool{!snapshot.text.is_empty()&&(snapshot.text!=self.baseline||snapshot.response_count>self.count)}
    pub fn new(snapshot: Snapshot, timeout: Duration) -> Self {
        let now = Instant::now();
        Self {
            baseline: snapshot.text,
            count: snapshot.response_count,
            last_text: String::new(),
            started: now,
            changed: now,
            timeout,
            saw_busy: false,
        }
    }
    pub fn update(&mut self, snapshot: Snapshot) -> Result<(String, bool)> {
        if !snapshot.error.is_empty() {
            bail!("{}", snapshot.error);
        }
        if !snapshot.url.is_empty() && !is_search_url(&snapshot.url) {
            bail!("Открыта другая страница. Вернитесь в Google AI Mode в Chrome.");
        }
        if self.started.elapsed() > self.timeout {
            bail!("Истекло время ожидания ответа AI Mode. Проверьте Chrome; Ctrl+N начинает новый диалог.");
        }
        self.saw_busy |= snapshot.busy;
        if snapshot.text.is_empty()
            || (snapshot.text == self.baseline && snapshot.response_count <= self.count)
        {
            return Ok((self.last_text.clone(), false));
        }
        if snapshot.text != self.last_text {
            self.last_text = snapshot.text;
            self.changed = Instant::now();
        }
        let stable = self.changed.elapsed();
        let done = !snapshot.busy
            && (snapshot.complete || self.saw_busy)
            && stable >= Duration::from_secs(2);
        Ok((self.last_text.clone(), done))
    }
}

pub struct AiMode {
    browser: Browser,
    page: PageConfig,
    response_timeout: Duration,
}
impl AiMode {
    pub async fn start(cfg: &Config) -> Result<Self> {
        let browser = Browser::start(cfg).await?;
        Ok(Self {
            browser,
            page: cfg.ai_mode.clone(),
            response_timeout: Duration::from_secs(cfg.response_timeout_secs.max(10)),
        })
    }
    async fn script(&mut self, operation: &str, text: &str) -> Result<serde_json::Value> {
        let script = format!(
            "({})({}, {}, {})",
            include_str!("ai_mode.js"),
            serde_json::to_string(&self.page)?,
            serde_json::to_string(operation)?,
            serde_json::to_string(text)?
        );
        self.browser.evaluate(&script).await
    }
    pub async fn snapshot(&mut self) -> Result<Snapshot> {
        let value=self.script("snapshot", "").await?;
        if let Some(wait)=self.browser.take_rate_limit(){bail!("RATE_LIMITED:{wait}: Google returned HTTP 429");}
        serde_json::from_value(value)
            .context("Некорректный ответ адаптера AI Mode")
    }
    pub async fn wait_ready(&mut self) -> Result<()> {
        let started = Instant::now();
        let mut ready_checks = 0;
        loop {
            let snapshot = self.snapshot().await?;
            if !snapshot.error.is_empty() {
                bail!("{}", snapshot.error);
            }
            if snapshot.ready {
                ready_checks += 1;
                if ready_checks >= 3 {
                    return Ok(());
                }
            } else {
                ready_checks = 0;
            }
            if started.elapsed() > Duration::from_secs(120) {
                bail!("Поле AI Mode не появилось. Проверьте доступность AI Mode и открытый Chrome. Вход в аккаунт не обязателен.");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    pub async fn send(&mut self, text: &str) -> Result<Turn> {
        self.send_inner(text,None).await
    }
    pub(crate) async fn send_after_completed(&mut self,text:&str,completed_text:&str)->Result<Turn>{self.send_inner(text,Some(completed_text)).await}
    async fn send_inner(&mut self,text:&str,completed_text:Option<&str>)->Result<Turn>{
        let _=self.browser.command("Emulation.setFocusEmulationEnabled",json!({"enabled":true})).await;
        let snapshot = self.snapshot().await?;
        if !snapshot.error.is_empty() {
            bail!("{}", snapshot.error);
        }
        if !snapshot.ready {
            bail!("Поле AI Mode недоступно. Проверьте Chrome.");
        }
        if snapshot.busy && !(completed_text==Some(snapshot.text.as_str())&&!snapshot.generating) {
            bail!("Страница ещё генерирует ответ. Дождитесь завершения или начните новый диалог.");
        }
        self.script("fill", text).await?;
        self.browser
            .command("Input.insertText", json!({"text":text}))
            .await?;
        self.script("verify", text).await?;
        tokio::time::sleep(Duration::from_millis(150)).await;
        // Real CDP keyboard events, rather than untrusted DOM KeyboardEvents.
        if self.script("submit", "").await?.as_bool() != Some(true) {
            self.browser
            .command(
                "Input.dispatchKeyEvent",
                json!({"type":"keyDown","key":"Enter","code":"Enter","windowsVirtualKeyCode":13,"text":"\r","unmodifiedText":"\r"}),
            )
            .await?;
            self.browser
                .command(
                    "Input.dispatchKeyEvent",
                    json!({"type":"keyUp","key":"Enter","code":"Enter","windowsVirtualKeyCode":13}),
                )
                .await?;
        }
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let current = self.snapshot().await?;
            if !current.error.is_empty() {
                bail!("{}", current.error);
            }
            if !current.draft
                || current.busy
                || current.response_count > snapshot.response_count
                || current.text != snapshot.text
            {
                return Ok(Turn::new(snapshot, self.response_timeout));
            }
        }
        bail!("Google не подтвердил отправку. Текст сохранён; проверьте вкладку и повторите.");
    }
    pub async fn new_chat(&mut self) -> Result<()> {
        self.navigate(START_URL).await
    }
    async fn navigate(&mut self, url: &str) -> Result<()> {
        let old_frame = self.browser.command("Page.getFrameTree", json!({})).await?;
        let old_loader = old_frame["frameTree"]["frame"]["loaderId"].clone();
        let navigation = self
            .browser
            .command("Page.navigate", json!({"url":url}))
            .await?;
        if let Some(error) = navigation.get("errorText") {
            bail!("Ошибка загрузки AI Mode: {error}");
        }
        // Wait for a new document, rather than a fixed sleep that may accept old input.
        for _ in 0..60 {
            let frame = self.browser.command("Page.getFrameTree", json!({})).await?;
            if frame["frameTree"]["frame"]["loaderId"] != old_loader {
                return self.wait_ready().await;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        bail!("Google не открыл новый диалог за 30 секунд")
    }
    pub async fn diagnostics(&mut self) -> Result<String> {
        Ok(serde_json::to_string_pretty(
            &self.script("diagnostics", "").await?,
        )?)
    }
    pub async fn conversation_url(&mut self) -> Result<String> {
        let value = self.browser.evaluate("location.href").await?;
        Ok(value.as_str().unwrap_or(START_URL).to_owned())
    }
    pub async fn open_chat(&mut self, url: &str) -> Result<()> {
        if !is_search_url(url) {
            bail!("Некорректный адрес сохранённого диалога");
        }
        self.navigate(url).await
    }
    pub async fn show_browser(&mut self) -> Result<()> {
        let info = self
            .browser
            .command("Browser.getWindowForTarget", json!({}))
            .await?;
        if let Some(id) = info["windowId"].as_i64() {
            self.browser
                .command(
                    "Browser.setWindowBounds",
                    json!({"windowId":id,"bounds":{"windowState":"normal"}}),
                )
                .await?;
        }
        self.browser.command("Page.bringToFront", json!({})).await?;
        Ok(())
    }
    pub fn from_browser(browser:Browser,cfg:&Config)->Self{Self{browser,page:cfg.ai_mode.clone(),response_timeout:Duration::from_secs(cfg.response_timeout_secs.max(10))}}
    pub async fn stop_generation(&mut self)->Result<bool>{let script=format!("(()=>{{const selector={};const b=Array.from(document.querySelectorAll(selector)).find(e=>e.getClientRects().length);if(b){{b.click();return true;}}return false;}})()",serde_json::to_string(&self.page.stop_selector)?);Ok(self.browser.evaluate(&script).await?.as_bool().unwrap_or(false))}
    pub async fn attach_files(&mut self,files:&[std::path::PathBuf])->Result<()> {
        if files.is_empty(){return Ok(());}
        let root=self.browser.command("DOM.getDocument",json!({"depth":-1,"pierce":true})).await?;
        let node=self.browser.command("DOM.querySelector",json!({"nodeId":root["root"]["nodeId"],"selector":"input[type=file]"})).await?;
        let id=node["nodeId"].as_i64().filter(|n|*n!=0).context("This anonymous AI Mode page does not expose a file-upload input. Text files can still be included as context; PDF/image upload is unavailable in this session.")?;
        let attrs=self.browser.command("DOM.getAttributes",json!({"nodeId":id})).await?;
        let multiple=attrs["attributes"].as_array().is_some_and(|a|a.chunks(2).any(|pair|pair[0]=="multiple"));
        if files.len()>1&&!multiple{bail!("This page accepts one native file at a time; choose a single PDF/image");}
        self.browser.command("DOM.setFileInputFiles",json!({"nodeId":id,"files":files})).await?;
        let names:Vec<_>=files.iter().filter_map(|p|p.file_name().and_then(|n|n.to_str())).collect();
        for _ in 0..60{tokio::time::sleep(Duration::from_millis(500)).await;let script=format!("(()=>{{const names={};const text=document.body.innerText;return names.every(n=>text.includes(n))&&!Array.from(document.querySelectorAll('[aria-busy=\"true\"], [role=\"progressbar\"]')).some(e=>e.getClientRects().length);}})()",serde_json::to_string(&names)?);if self.browser.evaluate(&script).await?.as_bool()==Some(true){return Ok(());}}
        bail!("File upload was not confirmed by the page within 30 seconds")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_lookalike_hosts() {
        assert!(is_search_url(START_URL));
        assert!(!is_search_url(
            "https://www.google.com.evil.test/search?udm=50"
        ));
        assert!(!is_search_url("https://evil.test/?google.com/search"));
    }
    #[test]
    fn does_not_replay_old_answer_or_complete_during_generation() {
        let mut turn = Turn::new(
            Snapshot {
                text: "old".into(),
                response_count: 1,
                ..Default::default()
            },
            Duration::from_secs(30),
        );
        assert_eq!(
            turn.update(Snapshot {
                text: "old".into(),
                response_count: 1,
                complete: true,
                ..Default::default()
            })
            .unwrap(),
            (String::new(), false)
        );
        assert_eq!(
            turn.update(Snapshot {
                text: "new".into(),
                response_count: 2,
                busy: true,
                complete: true,
                ..Default::default()
            })
            .unwrap(),
            ("new".into(), false)
        );
        turn.changed = Instant::now() - Duration::from_secs(3);
        assert!(
            turn.update(Snapshot {
                text: "new".into(),
                response_count: 2,
                complete: true,
                ..Default::default()
            })
            .unwrap()
            .1
        );
    }
    #[test]
    fn same_length_edits_reset_stability_and_timeout_is_reported() {
        let mut turn = Turn::new(Snapshot::default(), Duration::from_secs(30));
        turn.last_text = "abc".into();
        turn.changed = Instant::now() - Duration::from_secs(20);
        assert!(
            !turn
                .update(Snapshot {
                    text: "xyz".into(),
                    complete: true,
                    ..Default::default()
                })
                .unwrap()
                .1
        );
        turn.started = Instant::now() - Duration::from_secs(31);
        assert!(turn.update(Snapshot::default()).is_err());
    }
}
