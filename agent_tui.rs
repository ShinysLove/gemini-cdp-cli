//! Ratatui workbench backed by the same AgentManager as the desktop UI.
use crate::{agents::*, attachments::Attachment, browser::Config, tools::ApprovalRequest};
use anyhow::Result;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};
use std::{collections::BTreeMap, io, path::PathBuf};
struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
    }
}
pub fn slider_value(column: u16, rect: Rect) -> usize {
    let width = rect.width.saturating_sub(1).max(1);
    let position = column.saturating_sub(rect.x).min(width);
    1 + ((position as usize * 49 + width as usize / 2) / width as usize)
}
fn safe(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}
pub async fn run(cfg: Config) -> Result<()> {
    let root = std::env::current_dir()?;
    let prefs_path = root.join("data/agents.json");
    let prefs = AgentPreferences::load(&prefs_path)?;
    let bus = EventBus::default();
    let mut events = bus.subscribe();
    let manager = AgentManager::new(
        SessionManager::chrome(cfg, &prefs),
        prefs.clone(),
        root.join("data/agent_runs"),
        bus,
    );
    let mut input = String::new();
    let mut workspace = root.clone();
    let args: Vec<_> = std::env::args().collect();
    if let Some(index) = args.iter().position(|s| s == "--workspace") {
        if let Some(path) = args.get(index + 1) {
            workspace = PathBuf::from(path).canonicalize()?;
        }
    }
    let mut count = prefs.workers;
    let mut view: Option<RunView> = None;
    let mut pending = BTreeMap::<String, ApprovalRequest>::new();
    let mut attachments: Vec<Attachment> = vec![];
    let mut status="Enter: run · Tab: focus slider · Ctrl+A/D: approve/deny · /attach PATH · /workspace PATH · /apply · /plan PATH · /plan-ai REQUEST".to_owned();
    let mut slider_focus = false;
    let mut slider = Rect::default();
    let mut plan = None;
    let mut show_diff = false;
    let mut scroll = 0u16;
    let mut show_files = false;
    enable_raw_mode()?;
    let _guard = Guard;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut keys = EventStream::new();
    loop {
        terminal.draw(|frame|{
            let areas=Layout::default().direction(Direction::Vertical).margin(1).constraints([Constraint::Length(3),Constraint::Length(4),Constraint::Min(4),Constraint::Length(5),Constraint::Length(3)]).split(frame.area());
            frame.render_widget(Paragraph::new(format!("Workspace: {}",safe(&workspace.display().to_string()))).block(Block::default().borders(Borders::ALL).title(" EVIL GEMINI · Multi-agent coding ")),areas[0]);
            let inner=Rect::new(areas[1].x+2,areas[1].y+1,areas[1].width.saturating_sub(4),2);let width=inner.width.saturating_sub(26).max(2);slider=Rect::new(inner.x+12,inner.y,width,1);let marker=(count-1)*(width.saturating_sub(1))as usize/49;let track=(0..width as usize).map(|i|if i==marker{'●'}else if i<marker{'━'}else{'─'}).collect::<String>();
            let(active,waiting)=view.as_ref().map(|v|(v.active,v.waiting_capacity)).unwrap_or_default();
            frame.render_widget(Paragraph::new(vec![Line::from(vec![Span::styled(format!("Agents {:>2}   ",count),Style::default().fg(Color::LightMagenta)),Span::styled(track,Style::default().fg(if slider_focus{Color::White}else{Color::DarkGray}))]),Line::from(format!("Configured {count} · Active {active} · Waiting capacity {waiting} · Browser limit {} · Files {}",prefs.max_sessions,attachments.len()))]).block(Block::default().borders(Borders::ALL).title(" Workers 1–50 · ←/→, Home/End or mouse ")),areas[1]);
            let content=if let Some(request)=pending.values().next(){format!("EXPLICIT APPROVAL REQUIRED — Ctrl+A allows this exact action; Ctrl+D denies\nAgent: {}\n{}\nWorking directory: {}\n{}",request.agent_id,request.description,request.cwd.display(),serde_json::to_string_pretty(&request.action).unwrap_or_default())}else if show_files{attachments.iter().map(|a|format!("{} · {} bytes · {}\n{}",a.name,a.size,a.mime,a.preview)).collect::<Vec<_>>().join("\n\n")}else if show_diff{view.as_ref().map(|v|v.changes.iter().map(|c|c.diff()).collect::<Vec<_>>().join("\n")).unwrap_or_default()}else{view.as_ref().map(|v|format!("Run {} — {:?}\n{}\n\n{}",v.id,v.state,v.agents.iter().map(|a|format!("[{:?}] {} · step {} · {}{}",a.state,a.task.title,a.steps,a.progress,a.error.as_ref().map(|e|format!("\n  ERROR: {e}")).unwrap_or_default())).collect::<Vec<_>>().join("\n"),v.summary)).unwrap_or_else(||"Enter a development request. Tools run in isolated project copies. Host commands and applying source changes require your explicit approval.\n\nF4 attachments · F5 diff · PageUp/PageDown scroll · Esc cancel run · Ctrl+C exit".into())};
            frame.render_widget(Paragraph::new(safe(&content)).wrap(Wrap{trim:false}).scroll((scroll,0)).block(Block::default().borders(Borders::ALL).title(if !pending.is_empty(){" Approval "}else if show_diff{" Proposed diff "}else{" Agent tasks and results "})),areas[2]);
            frame.render_widget(Paragraph::new(safe(&input)).wrap(Wrap{trim:false}).block(Block::default().borders(Borders::ALL).title(if slider_focus{" Slider focused — Tab to edit request "}else{" Request / command "})),areas[3]);
            frame.render_widget(Paragraph::new(safe(&status)).wrap(Wrap{trim:false}),areas[4]);
        })?;
        tokio::select! {
            event=events.recv()=>match event {
                Ok(AgentEvent::ApplyStatus{message,..})=>status=message,
                Ok(AgentEvent::Run(run))=>view=Some(run),
                Ok(AgentEvent::Activity{..})=>{},
                Ok(AgentEvent::Approval(request))=>{pending.insert(request.id.clone(),request);scroll=0;},
                Ok(AgentEvent::ApprovalResolved{id,..})=>{pending.remove(&id);},
                Ok(AgentEvent::Progress{agent_id,message})=>status=format!("{agent_id}: {}",crate::tools::truncate(&message,300)),
                Ok(AgentEvent::Error(error))=>status=error,
                Ok(AgentEvent::Plan(value))=>{plan=Some(value);status="AI plan validated. Enter the task to run it, or /plan FILE to replace it.".into();},
                Ok(AgentEvent::Attachment{agent_id,name,progress})=>status=format!("{agent_id}: uploading {name} — {progress}%"),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>{view=manager.snapshot().await;pending=manager.approvals.pending().await.into_iter().map(|r|(r.id.clone(),r)).collect();},
                Err(_)=>break,
            },
            event=keys.next()=>match event {
                Some(Ok(Event::Paste(text)))=>{
                    let paths=crate::attachments::parse_paths(&text);
                    if !paths.is_empty()&&paths.iter().all(|p|p.is_file()) {for path in paths{if attachments.len()>=10{status="Attachment limit: 10".into();break;}match Attachment::prepare(path,root.join("data/attachments"),prefs.max_attachment_bytes).await{Ok(file)=>attachments.push(file),Err(e)=>status=e.to_string()}}}else{input.push_str(&text);}
                }
                Some(Ok(Event::Mouse(mouse)))if matches!(mouse.kind,MouseEventKind::Down(MouseButton::Left)|MouseEventKind::Drag(MouseButton::Left))&&mouse.row==slider.y=>{count=slider_value(mouse.column,slider);manager.set_workers(count).await?;manager.preferences.read().await.save(&prefs_path)?;}
                Some(Ok(Event::Key(key)))if key.kind!=KeyEventKind::Release=>{
                    let ctrl=key.modifiers.contains(KeyModifiers::CONTROL)&&!key.modifiers.contains(KeyModifiers::ALT);
                    if ctrl&&key.code==KeyCode::Char('c'){manager.cancel().await;break;}
                    if ctrl&&matches!(key.code,KeyCode::Char('a')|KeyCode::Char('d')){if let Some(id)=pending.keys().next().cloned(){manager.approvals.decide(&id,key.code==KeyCode::Char('a')).await;}continue;}
                    match key.code{
                        KeyCode::Tab=>slider_focus=!slider_focus,
                        KeyCode::Left|KeyCode::Right|KeyCode::Home|KeyCode::End if slider_focus=>{count=match key.code{KeyCode::Left=>count.saturating_sub(1).max(1),KeyCode::Right=>(count+1).min(50),KeyCode::Home=>1,_=>50};manager.set_workers(count).await?;manager.preferences.read().await.save(&prefs_path)?;}
                        KeyCode::F(4)=>{show_files=!show_files;show_diff=false;scroll=0;},
                        KeyCode::F(5)=>{show_diff=!show_diff;show_files=false;scroll=0;},
                        KeyCode::PageUp=>scroll=scroll.saturating_sub(10),KeyCode::PageDown=>scroll=scroll.saturating_add(10),
                        KeyCode::Esc=>{manager.cancel().await;status="Cancelling agents and pending approvals…".into();},
                        KeyCode::Backspace if !slider_focus=>{input.pop();},
                        KeyCode::Enter if !slider_focus=>{
                            let text=input.trim().to_owned();if text.is_empty(){continue;}
                            if let Some(path)=text.strip_prefix("/workspace "){if view.as_ref().is_some_and(|r|!r.state.terminal()){status="Cancel the current run first".into();}else{match PathBuf::from(path.trim_matches('"')).canonicalize(){Ok(path)if path.is_dir()=>workspace=path,_=>status="Workspace does not exist".into()}}}
                            else if let Some(paths)=text.strip_prefix("/attach "){for path in crate::attachments::parse_paths(paths){if attachments.len()>=10{break;}match Attachment::prepare(path,root.join("data/attachments"),prefs.max_attachment_bytes).await{Ok(file)=>attachments.push(file),Err(e)=>status=e.to_string()}}}
                            else if text=="/apply"{let manager=manager.clone();tokio::spawn(async move{if let Err(error)=manager.request_apply(tokio_util::sync::CancellationToken::new()).await{manager.bus.publish(AgentEvent::Error(error.to_string()));}});}
                            else if let Some(path)=text.strip_prefix("/plan "){match tokio::fs::read_to_string(path.trim_matches('"')).await{Ok(raw)=>match TaskPlanner::parse(&raw){Ok(value)=>{plan=Some(value);status="Validated plan loaded".into();},Err(error)=>status=error.to_string()},Err(error)=>status=error.to_string()}}
                            else if let Some(goal)=text.strip_prefix("/plan-ai "){let manager=manager.clone();let goal=goal.to_owned();let workspace=workspace.clone();tokio::spawn(async move{if let Err(error)=manager.generate_plan(&goal,workspace,&tokio_util::sync::CancellationToken::new()).await{manager.bus.publish(AgentEvent::Error(error.to_string()));}});}
                            else {let manager=manager.clone();let workspace=workspace.clone();let files=attachments.clone();let plan=plan.clone();tokio::spawn(async move{if let Err(error)=manager.start(text,workspace,files,plan).await{manager.bus.publish(AgentEvent::Error(error.to_string()));}});}
                            input.clear();
                        }
                        // Windows reports AltGr as Ctrl+Alt; those characters are text too.
                        KeyCode::Char(c) if !slider_focus&&(!key.modifiers.contains(KeyModifiers::CONTROL)||key.modifiers.contains(KeyModifiers::ALT))=>input.push(c),
                        _=>{}
                    }
                }
                Some(Err(error))=>return Err(error.into()),None=>break,_=>{}
            }
        }
    }
    manager.cancel().await;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slider_maps_edges_and_clamps() {
        let r = Rect::new(10, 3, 50, 1);
        assert_eq!(slider_value(0, r), 1);
        assert_eq!(slider_value(10, r), 1);
        assert_eq!(slider_value(59, r), 50);
        assert_eq!(slider_value(500, r), 50);
    }
}
