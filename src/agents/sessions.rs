use super::{events::*, model::*};
use crate::{
    ai_mode::AiMode,
    attachments::Attachment,
    browser::{Browser, Config},
};
use anyhow::{bail, Result};
use async_trait::async_trait;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
#[derive(Debug)]
pub enum ModelError {
    RateLimited(u64),
    Transient(String),
    Permanent(String),
    Cancelled,
}
impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ModelError {}
#[async_trait]
pub trait ModelSession: Send {
    async fn ask(
        &mut self,
        prompt: &str,
        attachments: &[Attachment],
        cancel: &CancellationToken,
        bus: &EventBus,
        agent: &str,
    ) -> Result<String, ModelError>;
    async fn close(&mut self) {}
}
#[async_trait]
pub trait SessionFactory: Send + Sync {
    async fn create(&self) -> Result<Box<dyn ModelSession>>;
}
#[derive(Clone)]
pub struct RequestGate {
    next: Arc<Mutex<tokio::time::Instant>>,
    paused: Arc<Mutex<tokio::time::Instant>>,
    interval: Duration,
}
impl RequestGate {
    pub async fn wait_resume(&self, cancel: &CancellationToken) -> Result<(), ModelError> {
        loop {
            let at = *self.paused.lock().await;
            if at <= tokio::time::Instant::now() {
                return Ok(());
            }
            tokio::select! {_=cancel.cancelled()=>return Err(ModelError::Cancelled),_=tokio::time::sleep_until(at)=>{}}
        }
    }
    pub fn new(seconds: u64) -> Self {
        Self {
            next: Arc::new(Mutex::new(tokio::time::Instant::now())),
            paused: Arc::new(Mutex::new(tokio::time::Instant::now())),
            interval: Duration::from_secs(seconds),
        }
    }
    pub async fn wait(&self, cancel: &CancellationToken) -> Result<(), ModelError> {
        let mut next = self.next.lock().await;
        loop {
            let at = (*next).max(*self.paused.lock().await);
            if at <= tokio::time::Instant::now() {
                *next = tokio::time::Instant::now() + self.interval;
                return Ok(());
            }
            tokio::select! {_=cancel.cancelled()=>return Err(ModelError::Cancelled),_=tokio::time::sleep_until(at)=>{}}
        }
    }
    pub async fn pause(&self, duration: Duration) {
        let mut paused = self.paused.lock().await;
        let now = tokio::time::Instant::now();
        let until = now
            .checked_add(duration)
            .unwrap_or_else(|| now + Duration::from_secs(365 * 86400));
        *paused = (*paused).max(until);
    }
}
#[derive(Clone)]
pub struct SessionManager {
    factory: Arc<dyn SessionFactory>,
    capacity: Arc<Semaphore>,
    pub gate: RequestGate,
    pub limit: usize,
}
pub struct SessionLease {
    pub session: Box<dyn ModelSession>,
    _permit: OwnedSemaphorePermit,
}
impl SessionManager {
    pub fn with_factory(factory: Arc<dyn SessionFactory>, prefs: &AgentPreferences) -> Self {
        Self {
            factory,
            capacity: Arc::new(Semaphore::new(prefs.max_sessions)),
            gate: RequestGate::new(prefs.min_request_interval_secs),
            limit: prefs.max_sessions,
        }
    }
    pub fn chrome(cfg: Config, prefs: &AgentPreferences) -> Self {
        Self::with_factory(
            Arc::new(ChromeFactory {
                cfg,
                bootstrap: Mutex::new(false),
                input: Arc::new(Mutex::new(())),
            }),
            prefs,
        )
    }
    pub async fn acquire(&self, cancel: &CancellationToken) -> Result<SessionLease> {
        let permit = tokio::select! {_=cancel.cancelled()=>bail!("Cancelled while waiting for session capacity"),permit=self.capacity.clone().acquire_owned()=>permit?};
        self.gate.wait_resume(cancel).await?;
        let session = tokio::select! {_=cancel.cancelled()=>bail!("Cancelled while creating session"),session=self.factory.create()=>session?};
        Ok(SessionLease {
            session,
            _permit: permit,
        })
    }
}
struct ChromeFactory {
    cfg: Config,
    bootstrap: Mutex<bool>,
    input: Arc<Mutex<()>>,
}
#[async_trait]
impl SessionFactory for ChromeFactory {
    async fn create(&self) -> Result<Box<dyn ModelSession>> {
        // The root WebSocket is owned by this lease. disposeOnDetach cleans up
        // even if cancellation happens before Chrome returns a context ID.
        {
            let mut bootstrap = self.bootstrap.lock().await;
            if !*bootstrap || Browser::browser_endpoint(&self.cfg).await.is_err() {
                if Browser::browser_endpoint(&self.cfg).await.is_err() {
                    drop(AiMode::start(&self.cfg).await?);
                }
                *bootstrap = true;
            }
        }
        let url = Browser::browser_endpoint(&self.cfg).await?;
        let mut browser = Browser::connect_url(&url).await?;
        let context = browser
            .command(
                "Target.createBrowserContext",
                json!({"disposeOnDetach":true}),
            )
            .await?["browserContextId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Anonymous context ID missing"))?
            .to_owned();
        let target = browser
            .command(
                "Target.createTarget",
                json!({"url":crate::ai_mode::START_URL,"browserContextId":context}),
            )
            .await?["targetId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Anonymous target ID missing"))?
            .to_owned();
        let root = Arc::new(Mutex::new(Some(browser)));
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()?;
        for _ in 0..60 {
            let values: Vec<serde_json::Value> = client
                .get(format!("http://127.0.0.1:{}/json", self.cfg.cdp_port))
                .send()
                .await?
                .json()
                .await?;
            if let Some(url) = values
                .iter()
                .find(|v| v["id"] == target)
                .and_then(|v| v["webSocketDebuggerUrl"].as_str())
            {
                let mut page = Browser::connect_url(url).await?;
                page.command("Network.enable", json!({})).await?;
                return Ok(Box::new(ChromeSession {
                    ai: AiMode::from_browser(page, &self.cfg),
                    context: Some(context),
                    root,
                    input: self.input.clone(),
                    uploaded: false,
                    last_completed: None,
                }));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        bail!("Anonymous tab CDP endpoint not found")
    }
}

struct StructuredCompletion {
    value: Option<serde_json::Value>,
    changed: std::time::Instant,
}
impl StructuredCompletion {
    fn new() -> Self {
        Self {
            value: None,
            changed: std::time::Instant::now(),
        }
    }
    fn observe(&mut self, snapshot: &crate::ai_mode::Snapshot, fresh: bool) -> bool {
        if !fresh {
            self.value = None;
            self.changed = std::time::Instant::now();
            return false;
        }
        let Ok(value) = super::protocol::decode::<serde_json::Value>(&snapshot.text) else {
            return false;
        };
        let envelope = (value["summary"].is_string()
            && value["done"].is_boolean()
            && value["actions"].is_array())
            || value["tasks"].is_array();
        if !envelope {
            return false;
        }
        if self.value.as_ref() != Some(&value) {
            self.value = Some(value);
            self.changed = std::time::Instant::now();
        }
        !snapshot.generating && self.changed.elapsed() >= Duration::from_secs(2)
    }
}
struct ChromeSession {
    ai: AiMode,
    context: Option<String>,
    root: Arc<Mutex<Option<Browser>>>,
    uploaded: bool,
    input: Arc<Mutex<()>>,
    last_completed: Option<serde_json::Value>,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structured_completion_requires_new_valid_stable_payload_and_idle_controls() {
        let mut completion = StructuredCompletion::new();
        let mut snapshot = crate::ai_mode::Snapshot {
            text: "```json\n{\"summary\":\"done\",\"done\":true,\"actions\":[]}\n```".into(),
            busy: true,
            ..Default::default()
        };
        assert!(!completion.observe(&snapshot, false));
        assert!(!completion.observe(&snapshot, true));
        completion.changed = std::time::Instant::now() - Duration::from_secs(3);
        snapshot.generating = true;
        assert!(!completion.observe(&snapshot, true));
        snapshot.generating = false;
        assert!(completion.observe(&snapshot, true));
        snapshot.text = "{\"summary\":".into();
        assert!(!completion.observe(&snapshot, true));
    }
    #[tokio::test]
    async fn shared_cooldown_can_be_cancelled_and_blocks_all_requesters() {
        let gate = RequestGate::new(0);
        gate.pause(Duration::from_secs(30)).await;
        let cancel = CancellationToken::new();
        let other = cancel.clone();
        let waiting = tokio::spawn(async move { gate.wait(&other).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiting.is_finished());
        cancel.cancel();
        assert!(matches!(waiting.await.unwrap(), Err(ModelError::Cancelled)));
    }
}
impl Drop for ChromeSession {
    fn drop(&mut self) {
        if let (Some(context), Ok(runtime)) =
            (self.context.take(), tokio::runtime::Handle::try_current())
        {
            let root = self.root.clone();
            runtime.spawn(async move {
                if let Some(browser) = root.lock().await.as_mut() {
                    let _ = browser
                        .command(
                            "Target.disposeBrowserContext",
                            json!({"browserContextId":context}),
                        )
                        .await;
                }
            });
        }
    }
}
fn classify(error: anyhow::Error) -> ModelError {
    let text = format!("{error:#}");
    if text.contains("RATE_LIMITED") {
        ModelError::RateLimited(
            text.split(':')
                .nth(1)
                .and_then(|n| n.parse().ok())
                .unwrap_or(0),
        )
    } else {
        if text.contains("не подтвердил отправку")
            || text.contains("ещё генерирует")
            || text.contains("Cannot find context")
            || text.contains("Execution context was destroyed")
        {
            ModelError::Transient(text)
        } else {
            ModelError::Permanent(text)
        }
    }
}
#[async_trait]
impl ModelSession for ChromeSession {
    async fn ask(
        &mut self,
        prompt: &str,
        attachments: &[Attachment],
        cancel: &CancellationToken,
        bus: &EventBus,
        agent: &str,
    ) -> Result<String, ModelError> {
        let result = async {
            self.ai.wait_ready().await.map_err(classify)?;
            let input_guard = self.input.lock().await;
            if !self.uploaded {
                let mut paths = vec![];
                for attachment in attachments.iter().filter(|a| a.native_upload) {
                    attachment.verify().await.map_err(classify)?;
                    bus.publish(AgentEvent::Attachment {
                        agent_id: agent.into(),
                        name: attachment.name.clone(),
                        progress: 0,
                    });
                    paths.push(attachment.stored_path.clone());
                }
                self.ai.attach_files(&paths).await.map_err(classify)?;
                for attachment in attachments.iter().filter(|a| a.native_upload) {
                    bus.publish(AgentEvent::Attachment {
                        agent_id: agent.into(),
                        name: attachment.name.clone(),
                        progress: 100,
                    });
                }
                self.uploaded = true;
            }
            let before = self.ai.snapshot().await.map_err(classify)?;
            let verified = self.last_completed.as_ref().is_some_and(|last| {
                !before.generating
                    && super::protocol::decode::<serde_json::Value>(&before.text)
                        .ok()
                        .as_ref()
                        == Some(last)
            });
            let mut turn = if verified {
                self.ai.send_after_completed(prompt, &before.text).await
            } else {
                self.ai.send(prompt).await
            }
            .map_err(classify)?;
            drop(input_guard);
            let mut last = String::new();
            let mut structured = StructuredCompletion::new();
            loop {
                let snapshot = self.ai.snapshot().await.map_err(classify)?;
                // A complete JSON envelope is an agent protocol boundary, even when
                // Google's code-only response omits the usual prose footer marker.
                if structured.observe(&snapshot, turn.is_new_response(&snapshot)) {
                    self.last_completed = super::protocol::decode(&snapshot.text).ok();
                    return Ok(snapshot.text);
                }
                let (text, done) = match turn.update(snapshot) {
                    Ok(value) => value,
                    Err(error) => {
                        bus.publish(AgentEvent::Activity {agent_id:agent.into(),role:"error".into(),content:format!("Response wait failed: {error:#}. Last visible response (not hidden reasoning): {}",crate::tools::truncate(&last,2000))});
                        return Err(classify(error));
                    }
                };
                if text != last {
                    last = text.clone();
                    bus.publish(AgentEvent::Progress {
                        agent_id: agent.into(),
                        message: crate::tools::truncate(&text, 600),
                    });
                }
                if done {
                    self.last_completed = super::protocol::decode(&text).ok();
                    return Ok(text);
                }
                tokio::time::sleep(Duration::from_millis(600)).await;
            }
        };
        tokio::select! {_=cancel.cancelled()=>{let _=self.ai.stop_generation().await;Err(ModelError::Cancelled)},value=result=>value}
    }
    async fn close(&mut self) {
        if let Some(context) = self.context.take() {
            if let Some(browser) = self.root.lock().await.as_mut() {
                let _ = browser
                    .command(
                        "Target.disposeBrowserContext",
                        json!({"browserContextId":context}),
                    )
                    .await;
            }
        }
    }
}
