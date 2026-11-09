use crate::{
    agents::{AgentEvent, AgentManager, AgentPreferences, EventBus, SessionManager},
    ai_mode::{AiMode, Turn},
    attachments::Attachment,
    browser::Config,
    history::{render_markdown, History, Message},
};
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{path::PathBuf, time::Duration};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::{Icon, Window, WindowBuilder},
};
use tokio::sync::mpsc::{channel, Receiver};
use wry::{WebContext, WebViewBuilder};

#[derive(Debug, Deserialize, Default)]
struct Command {
    action: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    blur: bool,
    #[serde(default)]
    font_size: u8,
    #[serde(default)]
    workers: usize,
    #[serde(default)]
    allowed: bool,
    #[serde(default)]
    files: Vec<String>,
}
enum UiEvent {
    State(Value),
    Native(String),
    Blur(bool),
    Toast(String),
    Agents(Value),
    Challenge(bool),
}

fn root_dir() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    if cwd.join("config.json").exists() {
        return Ok(cwd);
    }
    let exe = std::env::current_exe()?;
    for dir in exe.ancestors().skip(1).take(5) {
        if dir.join("config.json").exists() {
            return Ok(dir.to_path_buf());
        }
    }
    bail!("Рядом с приложением не найден config.json")
}

fn glass(window: &Window, enabled: bool) {
    if enabled {
        if window_vibrancy::apply_mica(window, Some(true)).is_err() {
            let _ = window_vibrancy::apply_acrylic(window, Some((19, 20, 26, 210)));
        }
    } else {
        let _ = window_vibrancy::clear_mica(window);
        let _ = window_vibrancy::clear_acrylic(window);
    }
}

fn app_icon() -> Option<Icon> {
    let size = 64u32;
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let px = (x as f32 + 0.5) / size as f32 * 2.0 - 1.0;
            let py = (y as f32 + 0.5) / size as f32 * 2.0 - 1.0;
            let corner =
                ((px.abs() - 0.67).max(0.0).powi(2) + (py.abs() - 0.67).max(0.0).powi(2)).sqrt();
            let alpha = ((0.31 - corner) * 64.0).clamp(0.0, 1.0);
            let star = ((0.98 - (px.abs().sqrt() + py.abs().sqrt())) * 55.0).clamp(0.0, 1.0);
            pixels.extend_from_slice(&[
                (24.0 + star * 201.0) as u8,
                (25.0 + star * 137.0) as u8,
                (31.0 + star * 140.0) as u8,
                (alpha * 255.0) as u8,
            ]);
        }
    }
    Icon::from_rgba(pixels, size, size).ok()
}

pub fn run() -> Result<()> {
    let root = root_dir()?;
    std::env::set_current_dir(&root)?;
    // One process owns the Google conversation and the history file.
    let listener = std::net::TcpListener::bind(("127.0.0.1", 49273))
        .context("EVIL GEMINI уже запущен. Откройте существующее окно приложения")?;
    let history = History::load(&root.join("data/history.json"))?;
    let initial_blur = history.settings.blur;
    let event_loop = EventLoopBuilder::<UiEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("EVIL GEMINI")
        .with_window_icon(app_icon())
        .with_inner_size(LogicalSize::new(1180.0, 800.0))
        .with_min_inner_size(LogicalSize::new(780.0, 580.0))
        .with_decorations(false)
        .with_transparent(true)
        .build(&event_loop)?;
    glass(&window, initial_blur);
    let (tx, rx) = channel::<Command>(64);
    let mut context = WebContext::new(Some(root.join("webview_data")));
    let html = include_str!("../ui/index.html")
        .replace(
            "/*__CSS__*/",
            &format!(
                "{}\n{}\n{}\n{}",
                include_str!("../ui/style.css"),
                include_str!("../ui/agents.css"),
                include_str!("../ui/liquid-glass.css"),
                include_str!("../ui/controls.css")
            ),
        )
        .replace(
            "/*__JS__*/",
            &format!(
                "{}\n{}\n{}",
                include_str!("../ui/app.js"),
                include_str!("../ui/workbench-core.js"),
                include_str!("../ui/agents.js")
            ),
        );
    let local_url = format!(
        "data:text/html;charset=utf-8;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(html.as_bytes())
    );
    let ipc_proxy = proxy.clone();
    let drop_tx = tx.clone();
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_transparent(false)
        .with_background_color((22, 22, 24, 255))
        .with_html(html)
        .with_navigation_handler(move |url| url == local_url || url == "about:blank")
        .with_drag_drop_handler(move |event|{if let wry::DragDropEvent::Drop{paths,..}=event{let _=drop_tx.try_send(Command{action:"attach_paths".into(),files:paths.iter().map(|p|p.to_string_lossy().into_owned()).collect(),..Default::default()});}true})
        .with_ipc_handler(move |request| {
            if request.body().len()>29_000_000{return;}
            if let Ok(command) = serde_json::from_str::<Command>(request.body()) {
                match command.action.as_str() {
                    "drag" | "minimize" | "maximize" | "close" => {
                        let _ = ipc_proxy.send_event(UiEvent::Native(command.action));
                    }
                    "copy" => {
                        let result = arboard::Clipboard::new()
                            .and_then(|mut clipboard| clipboard.set_text(command.text));
                        let _ = ipc_proxy.send_event(UiEvent::Toast(
                            if result.is_ok() {
                                "Ответ скопирован"
                            } else {
                                "Не удалось скопировать"
                            }
                            .into(),
                        ));
                    }
                    "open_link" => {
                        if let Ok(url) = reqwest::Url::parse(&command.text) {
                            if matches!(url.scheme(), "https" | "http") {
                                let _ = std::process::Command::new("rundll32.exe")
                                    .args(["url.dll,FileProtocolHandler", url.as_str()])
                                    .spawn();
                            }
                        }
                    }
                    "choose_workspace"|"choose_attachments"=>{
                        let tx=tx.clone();std::thread::spawn(move||{
                            let mut command=command;
                            if command.action=="choose_workspace"{if let Some(path)=rfd::FileDialog::new().set_title("Выбрать рабочий проект").pick_folder(){command.action="set_workspace".into();command.text=path.to_string_lossy().into_owned();let _=tx.blocking_send(command);}}
                            else if let Some(files)=rfd::FileDialog::new().set_title("Прикрепить файлы: они будут отправлены в Google с вашим запросом").pick_files(){command.action="attach_paths".into();command.files=files.iter().map(|p|p.to_string_lossy().into_owned()).collect();let _=tx.blocking_send(command);}
                        });
                    }
                    _ => {
                        if tx.try_send(command).is_err(){let _=ipc_proxy.send_event(UiEvent::Toast("Очередь команд заполнена; повторите позже".into()));}
                    }
                }
            }
        })
        .build(&window)?;
    std::thread::spawn(move || match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime.block_on(worker(root, history, rx, proxy)),
        Err(error) => eprintln!("{error}"),
    });
    event_loop.run(move |event, _, control| {
        // Keep the single-instance guard and WebView alive for the entire loop.
        let _ = &listener;
        *control = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control = ControlFlow::Exit,
            Event::UserEvent(UiEvent::State(state)) => {
                let _ =
                    webview.evaluate_script(&format!("window.receive && window.receive({state})"));
            }
            Event::UserEvent(UiEvent::Toast(text)) => {
                let _ = webview.evaluate_script(&format!(
                    "window.receive && window.receive({})",
                    json!({"toast":text})
                ));
            }
            Event::UserEvent(UiEvent::Blur(enabled)) => glass(&window, enabled),
            Event::UserEvent(UiEvent::Agents(value)) => {
                let _ = webview.evaluate_script(&format!(
                    "window.agentReceive && window.agentReceive({value})"
                ));
            }
            Event::UserEvent(UiEvent::Challenge(active)) => {
                let notice = if active { "Google запросил проверку (CAPTCHA или согласие на cookie). Завершите её в окне Chrome — приложение дождётся." } else { "Проверка завершена. Продолжаем работу." };
                let value = serde_json::to_string(notice).unwrap();
                let _ = webview.evaluate_script(&format!("window.challengeNotice({value}, {active})"));
                if !active { window.set_minimized(false); window.set_focus(); }
            }
            Event::UserEvent(UiEvent::Native(action)) => match action.as_str() {
                "close" => *control = ControlFlow::Exit,
                "minimize" => window.set_minimized(true),
                "maximize" => window.set_maximized(!window.is_maximized()),
                "drag" => {
                    let _ = window.drag_window();
                }
                _ => {}
            },
            _ => {}
        }
    });
}

struct Session {
    attachment_images: std::collections::HashMap<String, String>,
    history: History,
    phase: &'static str,
    status: String,
    error: String,
    turn: Option<Turn>,
    proxy: EventLoopProxy<UiEvent>,
    path: PathBuf,
}
impl Session {
    fn emit(&self, sent: bool) {
        let chats: Vec<Value> = self.history.chats.iter().map(|chat| json!({"id":chat.id,"title":chat.title,
            "messages":chat.messages.iter().map(|m|json!({"role":m.role,"text":m.text,"html":if m.role=="assistant"{render_markdown(&m.text)}else{String::new()},"interrupted":m.interrupted,"attachments":m.attachments.iter().map(|a|json!({"id":a.id,"name":a.name,"mime":a.mime,"image":self.attachment_images.get(&a.id)})).collect::<Vec<_>>()})).collect::<Vec<_>>() })).collect();
        let _ = self.proxy.send_event(UiEvent::State(json!({"chats":chats,"active_id":self.history.active_id,"settings":self.history.settings,"busy":self.turn.is_some(),"phase":self.phase,"status":self.status,"error":self.error,"sent":sent})));
    }
    fn save(&mut self) {
        if let Err(error) = self.history.save(&self.path) {
            self.error = format!("Не удалось сохранить диалог: {error:#}");
        }
    }
    fn fail(&mut self, error: anyhow::Error) {
        self.error = format!("{error:#}");
        self.status = "Требуется внимание".into();
        self.emit(false);
    }
    fn interrupt(&mut self) {
        if self.turn.take().is_some() {
            if let Some(message) = self
                .history
                .active_mut()
                .and_then(|c| c.messages.last_mut())
            {
                message.interrupted = true;
            }
            self.save();
        }
    }
}

async fn connect() -> Result<AiMode> {
    let cfg: Config = serde_json::from_slice(&std::fs::read("config.json")?)?;
    let mut provider = AiMode::start(&cfg).await?;
    provider.wait_ready().await?;
    Ok(provider)
}

async fn worker(
    root: PathBuf,
    history: History,
    mut rx: Receiver<Command>,
    proxy: EventLoopProxy<UiEvent>,
) {
    let mut session = Session {
        attachment_images: std::collections::HashMap::new(),
        history,
        phase: "connecting",
        status: "Открываем Google AI Mode".into(),
        error: String::new(),
        turn: None,
        proxy,
        path: root.join("data/history.json"),
    };
    if session.history.active().is_none() {
        session.history.new_chat();
    }
    for chat in &session.history.chats {
        for message in &chat.messages {
            for file in &message.attachments {
                if !session.attachment_images.contains_key(&file.id) {
                    if let Ok(Some(image)) = file.image_data_url().await { session.attachment_images.insert(file.id.clone(), image); }
                }
            }
        }
    }
    let mut provider: Option<AiMode> = None;
    let prefs_path = root.join("data/agents.json");
    let prefs = match AgentPreferences::load(&prefs_path) {
        Ok(prefs) => prefs,
        Err(error) => {
            session.fail(error);
            return;
        }
    };
    let config: Config = match std::fs::read(root.join("config.json"))
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(std::io::Error::other))
    {
        Ok(cfg) => cfg,
        Err(error) => {
            session.fail(error.into());
            return;
        }
    };
    let challenge_proxy = session.proxy.clone();
    crate::ai_mode::install_challenge_handler(std::sync::Arc::new(move |active| { let _ = challenge_proxy.send_event(UiEvent::Challenge(active)); }));
    let bus = EventBus::default();
    let mut agent_events = bus.subscribe();
    let manager = AgentManager::new(
        SessionManager::chrome(config, &prefs),
        prefs.clone(),
        root.join("data/agent_runs"),
        bus,
    );
    let navigation_path = root.join("data/workbench.json");
    let mut navigation =
        crate::workbench::Workbench::load(&navigation_path, &root).unwrap_or_default();
    if !navigation_path.exists() {
        if let Ok(index) = crate::workbench::run_index(&root.join("data/agent_runs")).await {
            if let Some(path) = index
                .as_array()
                .and_then(|runs| runs.first())
                .and_then(|run| run["workspace"].as_str())
            {
                let path = PathBuf::from(path);
                if path.is_dir() {
                    navigation.select(path);
                }
            }
        }
        let _ = navigation.save(&navigation_path);
    }
    if !navigation.workspace.is_dir() {
        navigation.select(root.clone());
    }
    let mut workspace = navigation.workspace.clone();
    let mut attachments: Vec<Attachment> = vec![];
    let terminal_busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let terminal_cancel = std::sync::Arc::new(tokio::sync::Mutex::new(
        tokio_util::sync::CancellationToken::new(),
    ));
    let mut poll = tokio::time::interval(Duration::from_millis(450));
    loop {
        // A dead CDP socket never recovers; drop it so "init"/"show_browser" reconnect.
        if provider.as_ref().is_some_and(|ai| !ai.is_connected()) {
            provider = None;
            session.interrupt();
            session.phase = "error";
            session.status = "Соединение с Chrome потеряно".into();
            session.error = "Соединение с Chrome потеряно. Нажмите значок Google AI Mode, чтобы переподключиться.".into();
            session.emit(false);
        }
        tokio::select! {
            command=rx.recv()=>{
                let Some(command)=command else {break};
                if command.action=="init_history"{session.status="Браузер подключится при запуске задачи".into();session.emit(false);continue;}
                if command.action=="agent_index" || command.action=="agent_init" {
                    let index=crate::workbench::run_index(&root.join("data/agent_runs")).await.unwrap_or(json!([]));
                    let _=session.proxy.send_event(UiEvent::Agents(json!({"type":"index","data":index,"projects":navigation.projects})));
                    if command.action=="agent_index"{continue;}
                }
                if command.action=="agent_init"{
                    let _=session.proxy.send_event(UiEvent::Agents(json!({"type":"preferences","data":&*manager.preferences.read().await,"workspace":workspace})));
                    if let Some(view)=manager.snapshot().await{let _=session.proxy.send_event(UiEvent::Agents(serde_json::to_value(AgentEvent::Run(view.event_snapshot())).unwrap()));}
                    for request in manager.approvals.pending().await{let _=session.proxy.send_event(UiEvent::Agents(serde_json::to_value(AgentEvent::Approval(request)).unwrap()));}continue;
                }
                if command.action=="set_access" {
                    match serde_json::from_value::<crate::tools::AccessMode>(json!(command.text)) {
                        Ok(mode)=>{manager.set_access_mode(mode).await;if let Err(error)=manager.preferences.read().await.save(&prefs_path){session.fail(error);}let _=session.proxy.send_event(UiEvent::Agents(json!({"type":"access","data":mode})));},
                        Err(error)=>manager.bus.publish(AgentEvent::Error(format!("Unknown access mode: {error}")))
                    }continue;
                }
                if command.action=="set_workers"{if let Err(error)=manager.set_workers(command.workers).await{session.fail(error);}else if let Err(error)=manager.preferences.read().await.save(&prefs_path){session.fail(error);}continue;}
                if command.action=="set_workspace"{
                    if manager.snapshot().await.is_some_and(|r|!r.state.terminal()){session.error="Сначала завершите или отмените текущую задачу".into();session.emit(false);continue;}
                    match std::fs::canonicalize(&command.text){Ok(path)if path.is_dir()=>{workspace=path;navigation.select(workspace.clone());if let Err(error)=navigation.save(&navigation_path){session.fail(error);}let _=session.proxy.send_event(UiEvent::Agents(json!({"type":"workspace","data":workspace})));},_=>session.fail(anyhow::anyhow!("Папка проекта не найдена"))}continue;
                }
                if command.action=="attach_clipboard" {
                    if attachments.len() >= 10 { session.error = "Не более 10 вложений".into(); }
                    else {
                        match Attachment::from_clipboard(&command.id, &command.text, root.join("data/attachments"), prefs.max_attachment_bytes.min(20 * 1024 * 1024)).await {
                            Ok(file) => attachments.push(file),
                            Err(error) => session.error = format!("Вставка изображения: {error:#}"),
                        }
                    }
                    let mut files = vec![];
                    for file in &attachments {
                        let mut value = serde_json::to_value(file).unwrap();
                        value["image"] = json!(file.image_data_url().await.ok().flatten());
                        files.push(value);
                    }
                    let _ = session.proxy.send_event(UiEvent::Agents(json!({"type":"attachments","data":files})));
                    session.emit(false); continue;
                }
                if command.action=="attach_paths"{
                    let paths=if command.files.is_empty(){crate::attachments::parse_paths(&command.text)}else{command.files.iter().map(PathBuf::from).collect()};
                    for path in paths{if attachments.len()>=10{session.error="Не более 10 вложений".into();break;}match Attachment::prepare(path,root.join("data/attachments"),prefs.max_attachment_bytes).await{Ok(file)=>attachments.push(file),Err(error)=>{session.error=format!("Вложение: {error:#}");}}}
                    let mut files=vec![];for file in &attachments{let mut value=serde_json::to_value(file).unwrap();value["image"]=json!(file.image_data_url().await.ok().flatten());files.push(value);}
                    let _=session.proxy.send_event(UiEvent::Agents(json!({"type":"attachments","data":files})));session.emit(false);continue;
                }
                if command.action=="remove_attachment"{
                    attachments.retain(|a|a.id!=command.id);
                    let mut files=vec![];
                    for file in &attachments {let mut value=serde_json::to_value(file).unwrap();value["image"]=json!(file.image_data_url().await.ok().flatten());files.push(value);}
                    let _=session.proxy.send_event(UiEvent::Agents(json!({"type":"attachments","data":files})));continue;
                }
                if command.action=="agent_open_run" || command.action=="agent_detail" {
                    let current=manager.snapshot().await;
                    let run=if current.as_ref().is_some_and(|r|r.id==command.id){current}else{crate::workbench::read_run(&root.join("data/agent_runs"),&command.id).await.ok()};
                    if let Some(run)=run {
                        if command.action=="agent_detail" {if let Some(agent)=run.agents.iter().find(|a|a.id==command.text){let _=session.proxy.send_event(UiEvent::Agents(json!({"type":"detail","data":agent})));}}
                        else {let _=session.proxy.send_event(UiEvent::Agents(json!({"type":"archive","data":run.event_snapshot(),"live":manager.snapshot().await.is_some_and(|r|r.id==command.id)})));}
                    }else{manager.bus.publish(AgentEvent::Error("Отчёт задачи не найден".into()));}continue;
                }
                if command.action=="agent_retry" {let manager=manager.clone();tokio::spawn(async move{if let Err(error)=manager.retry_failed(&command.id).await{manager.bus.publish(AgentEvent::Error(format!("Повтор: {error:#}")));}});continue;}
                if command.action=="terminal_cancel" {terminal_cancel.lock().await.cancel();continue;}
                if command.action=="terminal_run" {
                    let value=match serde_json::from_str::<Value>(&command.text){Ok(v)=>v,Err(error)=>{manager.bus.publish(AgentEvent::Error(format!("Команда должна быть JSON: {error}")));continue;}};
                    let program=value["program"].as_str().unwrap_or("").to_owned();
                    let args:Vec<String>=match serde_json::from_value(value["args"].clone()){Ok(args)=>args,Err(_)=>{manager.bus.publish(AgentEvent::Error("args должен быть массивом строк".into()));continue;}};
                    if program.is_empty()||program.len()>256||args.len()>100||args.iter().any(|a|a.len()>8000){manager.bus.publish(AgentEvent::Error("Недопустимый размер команды".into()));continue;}
                    if terminal_busy.swap(true,std::sync::atomic::Ordering::SeqCst){manager.bus.publish(AgentEvent::Error("Дождитесь предыдущей команды терминала".into()));continue;}
                    let busy=terminal_busy.clone();let manager=manager.clone();let workspace=workspace.clone();let storage=root.join("data/terminal");
                    let token=tokio_util::sync::CancellationToken::new();*terminal_cancel.lock().await=token.clone();
                    tokio::spawn(async move{
                        let id=crate::agents::model::id("terminal");
                        manager.bus.publish(AgentEvent::Activity{agent_id:id.clone(),role:"action".into(),content:json!({"tool":"run_command","program":program,"args":args}).to_string()});
                        let result=async{let snapshot=crate::tools::WorkspaceSnapshot::capture(workspace).await?;let mut tools=snapshot.isolate(storage.join(&id),&[]).await?;tools.approval=Some(manager.approvals.clone());tools.agent_id=id.clone();Ok::<_,anyhow::Error>(tools.execute(crate::tools::ToolCall::RunCommand{program,args},&token).await)}.await;
                        let content=match result{Ok(value)=>serde_json::to_string(&vec![value]).unwrap_or_default(),Err(error)=>json!([{"tool":"terminal","ok":false,"output":format!("{error:#}"),"exit_code":null}]).to_string()};
                        manager.bus.publish(AgentEvent::Activity{agent_id:id,role:"tool".into(),content});busy.store(false,std::sync::atomic::Ordering::SeqCst);
                    });continue;
                }
                if command.action=="agent_start"{
                    let plan=if command.text.trim().is_empty(){None}else{match crate::agents::TaskPlanner::parse(&command.text){Ok(plan)=>Some(plan),Err(error)=>{manager.bus.publish(AgentEvent::Error(format!("Plan rejected: {error:#}")));continue;}}};
                    let goal=command.id;let manager=manager.clone();let workspace=workspace.clone();let files=attachments.clone();tokio::spawn(async move{if let Err(error)=manager.start(goal,workspace,files,plan).await{manager.bus.publish(AgentEvent::Error(format!("{error:#}")));}});continue;
                }
                if command.action=="agent_plan"{let manager=manager.clone();let workspace=workspace.clone();tokio::spawn(async move{if let Err(error)=manager.generate_plan(&command.text,workspace,&tokio_util::sync::CancellationToken::new()).await{manager.bus.publish(AgentEvent::Error(format!("Planner: {error:#}")));}});continue;}
                if command.action=="agent_cancel"{manager.cancel().await;continue;}
                if command.action=="cancel_agent"{manager.cancel_agent(&command.id).await;continue;}
                if command.action=="approval"{manager.approvals.decide(&command.id,command.allowed).await;continue;}
                if command.action=="agent_apply"{let manager=manager.clone();tokio::spawn(async move{if let Err(error)=manager.request_apply_run(&command.id,tokio_util::sync::CancellationToken::new()).await{manager.bus.publish(AgentEvent::Error(format!("Применение: {error:#}")));}});continue;}
                if command.action=="init" || (command.action=="show_browser" && provider.is_none()) {
                    session.emit(false);
                    if provider.is_none() {
                        match connect().await {
                            Ok(mut connected)=>{
                                let proxy = session.proxy.clone();
                                connected.set_challenge_handler(std::sync::Arc::new(move |active| { let _ = proxy.send_event(UiEvent::Challenge(active)); }));
                                let url=session.history.active().map(|c|c.url.clone()).unwrap_or_default();
                                let result=if url.is_empty(){connected.new_chat().await}else{connected.open_chat(&url).await};
                                provider=Some(connected);
                                session.phase="ready";session.status="Google AI Mode подключён".into();session.error.clear();
                                if let Err(error)=result{session.phase="error";session.error=format!("Не удалось восстановить вкладку: {error:#}");}
                                session.emit(false);
                            }
                            Err(error)=>{session.phase="error";session.fail(error);}
                        }
                    }
                    continue;
                }
                if command.action=="settings" {
                    session.history.settings.blur=command.blur;
                    session.history.settings.font_size=match command.font_size{14|15|17=>command.font_size,_=>15};
                    session.save();let _=session.proxy.send_event(UiEvent::Blur(command.blur));session.emit(false);continue;
                }
                if command.action=="stop" {session.interrupt();session.status="Ожидание прекращено. Google может продолжать генерацию.".into();session.emit(false);continue;}
                if command.action=="select" && provider.is_none() {
                    if session.history.chats.iter().any(|chat|chat.id==command.id) {session.history.active_id=command.id;session.save();session.emit(false);}
                    continue;
                }
                let Some(ref mut ai)=provider else {session.error="Подключение ещё не готово. Войдите в Google в окне Chrome и нажмите значок Google AI Mode для повторной попытки.".into();session.emit(false);continue;};
                match command.action.as_str() {
                    "show_browser"=>{if let Err(error)=ai.show_browser().await{session.fail(error);}}
                    "new"=>{
                        session.interrupt();session.error.clear();session.phase="connecting";session.status="Открываем новый диалог".into();session.emit(false);
                        match ai.new_chat().await {
                            Ok(())=>{session.history.new_chat();session.phase="ready";session.status="Новый диалог готов".into();session.save();session.emit(false);}
                            Err(error)=>{session.phase="error";session.fail(error);}
                        }
                    }
                    "select"=>{
                        if command.id==session.history.active_id {continue;}
                        let Some(chat)=session.history.chats.iter().find(|chat|chat.id==command.id) else {continue;};
                        let url=chat.url.clone();let has_messages=!chat.messages.is_empty();
                        session.interrupt();session.history.active_id=command.id.clone();session.save();session.phase="connecting";session.status="Восстанавливаем диалог".into();session.emit(false);
                        let result=if url.is_empty(){if has_messages {Err(anyhow::anyhow!("У диалога нет сохранённой ссылки Google. Можно читать историю, но продолжить этот диалог пока нельзя."))} else {ai.new_chat().await}}else{ai.open_chat(&url).await};
                        match result {
                            Ok(())=>{session.error.clear();session.phase="ready";session.status="Google AI Mode подключён".into();session.save();session.emit(false);}
                            Err(error)=>{session.phase="error";session.fail(error);}
                        }
                    }
                    "send"|"retry"=>{
                        if session.phase!="ready" { session.error="Сначала откройте новый диалог или восстановите подключение к Google.".into();session.emit(false);continue; }
                        if session.turn.is_some(){continue;}
                        let retry=command.action=="retry";
                        let mut text=if retry {session.history.active().and_then(|c|c.messages.iter().rev().find(|m|m.role=="user")).map(|m|m.text.clone()).unwrap_or_default()} else {command.text.trim().to_owned()};
                        if text.len()>240_000 {session.error="Сообщение слишком длинное".into();session.emit(false);continue;}
                        session.error.clear();
                        let sent_attachments = if retry {session.history.active().and_then(|c|c.messages.iter().rev().find(|m|m.role=="user")).map(|m|m.attachments.clone()).unwrap_or_default()} else {attachments.clone()};
                        if text.is_empty() {
                            if sent_attachments.is_empty() { continue; }
                            text = "Изучи приложенные файлы.".into();
                        }
                        let mut attachment_error=None;for attachment in &sent_attachments{if let Err(error)=attachment.verify().await{attachment_error=Some(error);break;}}
                        if let Some(error)=attachment_error{session.fail(error);continue;}
                        let native:Vec<_>=sent_attachments.iter().filter(|a|a.native_upload).map(|a|a.stored_path.clone()).collect();
                        if let Err(error)=ai.attach_files(&native).await{session.fail(error);continue;}
                        let mut context = String::new();
                        let mut context_error = None;
                        for file in sent_attachments.iter().filter(|a|!a.native_upload) {
                            match tokio::fs::read_to_string(&file.stored_path).await {
                                Ok(content) => context.push_str(&format!("\nUser-selected file {} (data):\n{}", file.name, content)),
                                Err(error) => { context_error = Some(error); break; }
                            }
                        }
                        if let Some(error) = context_error { session.fail(error.into()); continue; }
                        let sent_text=format!("{text}\n{context}");
                        match ai.send(&sent_text).await {
                            Ok(turn)=>{
                                for file in &sent_attachments {
                                    if let Ok(Some(image)) = file.image_data_url().await { session.attachment_images.insert(file.id.clone(), image); }
                                }
                                if let Some(chat)=session.history.active_mut() {
                                    if retry {if chat.messages.last().is_some_and(|m|m.role=="assistant"){chat.messages.pop();}}
                                    else {chat.messages.push(Message {role:"user".into(),text:text.clone(),interrupted:false,attachments:sent_attachments.clone()});}
                                    if chat.title=="Новый диалог"{chat.title=text.replace('\n'," ").chars().take(45).collect();}
                                    chat.messages.push(Message {role:"assistant".into(),text:String::new(),interrupted:false,attachments:vec![]});
                                }
                                session.turn=Some(turn);session.status="Google AI Mode отвечает".into();session.save();session.emit(!retry);if !retry {attachments.clear();let _=session.proxy.send_event(UiEvent::Agents(json!({"type":"attachments","data":[]})));}
                            }
                            Err(error)=>session.fail(error),
                        }
                    }
                    _=>{}
                }
            }
            event=agent_events.recv()=>{
                match event{Ok(event)=>{let _=session.proxy.send_event(UiEvent::Agents(serde_json::to_value(event).unwrap()));},Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>{if let Some(view)=manager.snapshot().await{let _=session.proxy.send_event(UiEvent::Agents(serde_json::to_value(AgentEvent::Run(view.event_snapshot())).unwrap()));}for request in manager.approvals.pending().await{let _=session.proxy.send_event(UiEvent::Agents(serde_json::to_value(AgentEvent::Approval(request)).unwrap()));}},Err(_)=>{}}
            }
            _=poll.tick(), if session.turn.is_some()=>{
                let Some(ref mut ai)=provider else {continue;};
                let result=match ai.snapshot().await {Ok(snapshot)=>session.turn.as_mut().unwrap().update(snapshot),Err(error)=>Err(error)};
                match result {
                    Ok((text,done))=>{
                        let mut changed=false;
                        if let Some(message)=session.history.active_mut().and_then(|c|c.messages.last_mut()) {
                            if message.text!=text{message.text=text;changed=true;}
                        }
                        if done {
                            session.turn=None;session.status="Google AI Mode подключён".into();
                            if let Ok(url)=ai.conversation_url().await {if let Some(chat)=session.history.active_mut(){chat.url=url;}}
                            session.save();session.emit(false);
                        } else if changed { session.save();session.emit(false); }
                    }
                    Err(error)=>{session.interrupt();session.fail(error);}
                }
            }
        }
    }
    session.interrupt();
    session.save();
}
