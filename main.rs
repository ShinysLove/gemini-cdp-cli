mod markdown;
use gemini_cdp_cli::{ai_mode, browser};

use anyhow::{Context, Result};
use arboard::Clipboard;
use crossterm::{
    event::{
        DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use std::{io, time::Duration};

struct Message {
    user: bool,
    text: String,
}
struct State {
    input: String,
    messages: Vec<Message>,
    status: String,
    scroll: u16,
    turn: Option<ai_mode::Turn>,
}

// Restores the terminal on every return path, including network/render errors.
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config: browser::Config = serde_json::from_str(
        &std::fs::read_to_string("config.json").context("Не найден config.json в рабочей папке")?,
    )?;
    if std::env::args().any(|arg|arg=="--live-agent-smoke"){return gemini_cdp_cli::agents::smoke::live(config).await;}
    if std::env::args().any(|arg|arg=="--agents"){return gemini_cdp_cli::agent_tui::run(config).await;}
    println!("Открываю Google Search AI Mode. Если потребуется, войдите в Google в окне Chrome.");
    let mut provider = ai_mode::AiMode::start(&config).await?;
    if std::env::args().any(|arg| arg == "--diagnose") {
        println!("{}", provider.diagnostics().await?);
        return Ok(());
    }
    provider.wait_ready().await?;
    println!("AI Mode готов. Открываю чистый диалог…");
    provider.new_chat().await?;
    // Explicit opt-in live integration check with two fixed, non-sensitive prompts.
    if std::env::args().any(|arg| arg == "--smoke-test") {
        for (prompt, expected) in [
            (
                "Remember the token BLUEORBIT for my next question. Reply with exactly AIMODEOK.",
                "AIMODEOK",
            ),
            (
                "What token did I ask you to remember? Reply with the token only.",
                "BLUEORBIT",
            ),
        ] {
            println!("Проверка диалога: ожидаю {expected}");
            let mut turn = provider.send(prompt).await?;
            loop {
                let (text, done) = turn.update(provider.snapshot().await?)?;
                if done {
                    println!("{text}");
                    anyhow::ensure!(text.contains(expected), "AI Mode вернул неожиданный ответ");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        return Ok(());
    }
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut state = State {
        input: String::new(),
        messages: vec![],
        status: "Готов · Google AI Mode · текстовый диалог".into(),
        scroll: 0,
        turn: None,
    };
    let mut events = EventStream::new();
    let mut poll = tokio::time::interval(Duration::from_millis(500));
    loop {
        terminal.draw(|frame| {
            let areas = Layout::default().direction(Direction::Vertical).margin(1)
                .constraints([Constraint::Length(3), Constraint::Min(3), Constraint::Length(5), Constraint::Length(2)]).split(frame.area());
            frame.render_widget(Paragraph::new(state.status.as_str()).block(Block::default().borders(Borders::ALL).title(" Google Search AI Mode · CDP ")), areas[0]);
            let width = areas[1].width.saturating_sub(4).max(4) as usize;
            let mut lines: Vec<Line<'static>> = vec![];
            for message in &state.messages {
                lines.push(Line::from(Span::styled(if message.user { "Вы" } else { "AI Mode" }, Style::default().fg(if message.user { Color::Cyan } else { Color::Blue }))));
                lines.extend(markdown::render_markdown(&message.text, width));
                lines.push(Line::from(""));
            }
            let max_scroll = lines.len().saturating_sub(areas[1].height.saturating_sub(2) as usize).min(u16::MAX as usize) as u16;
            state.scroll = state.scroll.min(max_scroll);
            frame.render_widget(Paragraph::new(lines).scroll((max_scroll.saturating_sub(state.scroll), 0)).block(Block::default().borders(Borders::ALL).title(" Диалог ")), areas[1]);
            let mut input = Vec::new();
            for part in state.input.split('\n') {
                input.extend(markdown::wrap_line(vec![Span::raw(part.to_owned())], 0, areas[2].width.saturating_sub(3).max(4) as usize));
            }
            let skip = input.len().saturating_sub(3);
            let cursor_x = input.last().map(|line| line.width()).unwrap_or(0) as u16;
            let cursor_y = input.len().saturating_sub(skip + 1) as u16;
            frame.render_widget(Paragraph::new(input).scroll((skip.min(u16::MAX as usize) as u16, 0)).block(Block::default().borders(Borders::ALL).title(" Сообщение ")), areas[2]);
            frame.set_cursor_position((areas[2].x + 1 + cursor_x.min(areas[2].width.saturating_sub(3)), areas[2].y + 1 + cursor_y.min(2)));
            frame.render_widget(Paragraph::new("Enter: отправить · Ctrl+J: строка · Ctrl+V: вставить текст\nCtrl+N: новый диалог · Esc: прекратить ожидание · Ctrl+C: выход · ↑↓: прокрутка"), areas[3]);
        })?;
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Paste(text))) => state.input.push_str(&text),
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                    match (key.code, key.modifiers) {
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => break,
                        (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                            state.turn = None;
                            state.status = "Открываю новый диалог…".into();
                            match provider.new_chat().await {
                                Ok(()) => {state.messages.clear(); state.scroll = 0; state.status = "Новый диалог готов".into();},
                                Err(error) => state.status = format!("Ошибка: {error:#}"),
                            }
                        },
                        (KeyCode::Esc, _) => { state.turn = None; state.status = "Ожидание прекращено. Генерация в Chrome может продолжаться.".into(); },
                        (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                            if let Ok(mut clipboard) = Clipboard::new() {
                                match clipboard.get_text() {
                                    Ok(text) => state.input.push_str(&text),
                                    Err(_) => state.status = "Пока поддерживается только текст; изображения не отправляются.".into(),
                                }
                            }
                        },
                        (KeyCode::Enter, m) if m.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL) => state.input.push('\n'),
                        (KeyCode::Char('j'), KeyModifiers::CONTROL) => state.input.push('\n'),
                        (KeyCode::Enter, KeyModifiers::NONE) if state.turn.is_none() && !state.input.trim().is_empty() => {
                            let prompt = state.input.trim().to_owned();
                            match provider.send(&prompt).await {
                                Ok(turn) => {
                                    state.messages.push(Message {user:true,text:prompt});
                                    state.messages.push(Message {user:false,text:String::new()});
                                    state.turn = Some(turn); state.input.clear(); state.scroll = 0;
                                    state.status = "AI Mode отвечает…".into();
                                },
                                Err(error) => state.status = format!("Отправка не удалась: {error:#}"),
                            }
                        },
                        (KeyCode::Backspace, _) => {state.input.pop();},
                        (KeyCode::Up, _) => state.scroll = state.scroll.saturating_add(2),
                        (KeyCode::Down, _) => state.scroll = state.scroll.saturating_sub(2),
                        // Windows reports AltGr as Ctrl+Alt; those characters are text too.
                        (KeyCode::Char(c), m) if !m.contains(KeyModifiers::CONTROL) || m.contains(KeyModifiers::ALT) => state.input.push(c),
                        _ => {}
                    }
                },
                Some(Err(error)) => return Err(error.into()),
                None => break,
                _ => {}
            },
            _ = poll.tick(), if state.turn.is_some() => {
                let result = match provider.snapshot().await {
                    Ok(snapshot) => state.turn.as_mut().unwrap().update(snapshot),
                    Err(error) => Err(error),
                };
                match result {
                    Ok((text, done)) => {
                        if let Some(message) = state.messages.last_mut() { message.text = text; }
                        if done {state.turn = None; state.status = "Готов · Google AI Mode".into();}
                    },
                    Err(error) => {state.turn = None; state.status = format!("Ошибка: {error:#}");}
                }
            }
        }
    }
    Ok(())
}
