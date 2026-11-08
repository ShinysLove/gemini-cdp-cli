use async_trait::async_trait;
use gemini_cdp_cli::{
    agents::{
        sessions::{ModelError, ModelSession, SessionFactory},
        *,
    },
    attachments::Attachment,
};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
struct Factory {
    live: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    delay: Duration,
    invalid: bool,
}
struct Session {
    live: Arc<AtomicUsize>,
    delay: Duration,
    round: usize,
    invalid: bool,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl SessionFactory for Factory {
    async fn create(&self) -> anyhow::Result<Box<dyn ModelSession>> {
        let n = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(n, Ordering::SeqCst);
        Ok(Box::new(Session {
            live: self.live.clone(),
            delay: self.delay,
            round: 0,
            invalid: self.invalid,
        }))
    }
}
#[async_trait]
impl ModelSession for Session {
    async fn ask(
        &mut self,
        prompt: &str,
        _: &[Attachment],
        cancel: &CancellationToken,
        _: &EventBus,
        _: &str,
    ) -> Result<String, ModelError> {
        tokio::select! {_=cancel.cancelled()=>return Err(ModelError::Cancelled),_=tokio::time::sleep(self.delay)=>{}}
        self.round += 1;
        if self.invalid {
            return Ok("not structured JSON".into());
        }
        let reply = if self.round == 1 && prompt.contains("WRITE:") {
            let target = prompt
                .split("WRITE:")
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap();
            let value = if prompt.contains("VALUE:B") { "B" } else { "A" };
            serde_json::json!({"summary":"Stage a file","done":false,"actions":[{"tool":"write_file","path":target,"content":value}]})
        } else if self.round == 1 && prompt.contains("CHECK:") {
            serde_json::json!({"summary":"Inspect dependency file","done":false,"actions":[{"tool":"read_file","path":"a.txt"}]})
        } else if self.round == 1 {
            serde_json::json!({"summary":"Inspect actual files","done":false,"actions":[{"tool":"list_directory","path":"."}]})
        } else {
            serde_json::json!({"summary":if prompt.contains("\"output\":\"A\""){"Dependency contents verified"}else{"Completed"},"done":true,"actions":[]})
        };
        Ok(reply.to_string())
    }
}
fn task(id: &str, instruction: &str, deps: &[&str]) -> TaskSpec {
    TaskSpec {
        id: id.into(),
        title: id.into(),
        instruction: instruction.into(),
        dependencies: deps.iter().map(|s| s.to_string()).collect(),
        priority: 0,
    }
}
fn setup(
    storage: &std::path::Path,
    limit: usize,
    workers: usize,
    invalid: bool,
    delay: u64,
) -> (AgentManager, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(Factory {
        live: live.clone(),
        peak: peak.clone(),
        delay: Duration::from_millis(delay),
        invalid,
    });
    let prefs = AgentPreferences {
        workers,
        max_sessions: limit,
        min_request_interval_secs: 0,
        max_retries: 1,
        ..Default::default()
    };
    let sessions = SessionManager::with_factory(factory, &prefs);
    (
        AgentManager::new(sessions, prefs, storage.into(), EventBus::default()),
        live,
        peak,
    )
}
async fn finish(manager: &AgentManager) -> RunView {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(view) = manager.snapshot().await {
                if view.state.terminal() {
                    return view;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("scheduler deadlocked")
}
struct LimitedFactory(Arc<AtomicUsize>);
struct RetryFactory(Arc<AtomicUsize>);
#[async_trait]
impl SessionFactory for RetryFactory {
    async fn create(&self) -> anyhow::Result<Box<dyn ModelSession>> {
        let index = self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Session {
            live: Arc::new(AtomicUsize::new(1)),
            delay: Duration::from_millis(5),
            round: 0,
            invalid: index == 1,
        }))
    }
}
#[tokio::test]
async fn retry_preserves_successful_artifacts_and_does_not_repeat_their_sessions() {
    let source = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let prefs = AgentPreferences {
        workers: 1,
        max_sessions: 1,
        max_retries: 0,
        min_request_interval_secs: 0,
        ..Default::default()
    };
    let sessions = SessionManager::with_factory(Arc::new(RetryFactory(count.clone())), &prefs);
    let manager = AgentManager::new(sessions, prefs, storage.path().into(), EventBus::default());
    let id = manager
        .start(
            "Retry".into(),
            source.path().into(),
            vec![],
            Some(TaskPlan {
                tasks: vec![
                    task("a", "WRITE:a.txt", &[]),
                    task("b", "WRITE:b.txt", &["a"]),
                ],
            }),
        )
        .await
        .unwrap();
    let first = finish(&manager).await;
    assert_eq!(first.state, ExecutionState::Failed);
    let original_id = first.agents[0].id.clone();
    tokio::time::sleep(Duration::from_millis(30)).await;
    manager.retry_failed(&id).await.unwrap();
    let retried = finish(&manager).await;
    assert_eq!(retried.state, ExecutionState::Succeeded);
    assert_eq!(retried.agents[0].id, original_id);
    assert_eq!(count.load(Ordering::SeqCst), 3);
    assert_eq!(retried.changes.len(), 2);
    assert!(!source.path().join("a.txt").exists());
}
struct LimitedSession;
#[async_trait]
impl SessionFactory for LimitedFactory {
    async fn create(&self) -> anyhow::Result<Box<dyn ModelSession>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(LimitedSession))
    }
}
#[async_trait]
impl ModelSession for LimitedSession {
    async fn ask(
        &mut self,
        _: &str,
        _: &[Attachment],
        _: &CancellationToken,
        _: &EventBus,
        _: &str,
    ) -> Result<String, ModelError> {
        Err(ModelError::RateLimited(120))
    }
}
#[tokio::test]
async fn queued_agent_can_be_cancelled_before_it_gets_a_session() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let (manager, _, _) = setup(data.path(), 1, 1, false, 150);
    manager
        .start(
            "Cancel queued".into(),
            source.path().into(),
            vec![],
            Some(TaskPlan {
                tasks: vec![
                    task("first", "inspect", &[]),
                    task("queued", "WRITE:never.txt", &["first"]),
                ],
            }),
        )
        .await
        .unwrap();
    let id = manager.snapshot().await.unwrap().agents[1].id.clone();
    assert!(manager.cancel_agent(&id).await);
    let view = finish(&manager).await;
    assert_eq!(view.agents[1].state, ExecutionState::Cancelled);
    assert!(view.changes.is_empty());
}
#[tokio::test]
async fn quota_pause_retains_session_and_cancellation_does_not_rotate_it() {
    let source = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let created = Arc::new(AtomicUsize::new(0));
    let prefs = AgentPreferences {
        workers: 50,
        max_sessions: 1,
        min_request_interval_secs: 0,
        ..Default::default()
    };
    let sessions = SessionManager::with_factory(Arc::new(LimitedFactory(created.clone())), &prefs);
    let manager = AgentManager::new(sessions, prefs, storage.path().into(), EventBus::default());
    manager
        .start(
            "Quota".into(),
            source.path().into(),
            vec![],
            Some(TaskPlan {
                tasks: vec![
                    task("first", "inspect", &[]),
                    task("second", "inspect", &[]),
                ],
            }),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if manager
                .snapshot()
                .await
                .unwrap()
                .agents
                .iter()
                .any(|a| a.state == ExecutionState::RateLimited)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(created.load(Ordering::SeqCst), 1);
    manager.cancel().await;
    assert_eq!(finish(&manager).await.state, ExecutionState::Cancelled);
    assert_eq!(created.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn parallel_dag_isolated_writes_and_dependency_context() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("README.md"), "project").unwrap();
    let (manager, live, peak) = setup(data.path(), 2, 3, false, 25);
    manager
        .start(
            "Implement".into(),
            source.path().into(),
            vec![],
            Some(TaskPlan {
                tasks: vec![
                    task("a", "WRITE:a.txt", &[]),
                    task("b", "WRITE:b.txt", &[]),
                    task("join", "CHECK:a", &["a", "b"]),
                ],
            }),
        )
        .await
        .unwrap();
    let result = finish(&manager).await;
    assert_eq!(
        result.state,
        ExecutionState::Succeeded,
        "{}",
        result.summary
    );
    assert_eq!(result.changes.len(), 2);
    assert!(!source.path().join("a.txt").exists());
    assert_eq!(live.load(Ordering::SeqCst), 0);
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    assert!(result.agents[2]
        .output
        .as_ref()
        .unwrap()
        .summary
        .contains("Dependency contents verified"));
    assert!(result.agents.iter().all(|a| !a.history.is_empty()));
}
#[tokio::test]
async fn cancellation_drains_workers_and_blocks_dependents_without_touching_source() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let (manager, live, _) = setup(data.path(), 2, 50, false, 1000);
    manager
        .start(
            "Cancel".into(),
            source.path().into(),
            vec![],
            Some(TaskPlan {
                tasks: vec![
                    task("a", "WRITE:a.txt", &[]),
                    task("b", "WRITE:b.txt", &["a"]),
                ],
            }),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    manager.cancel().await;
    let view = finish(&manager).await;
    assert_eq!(view.state, ExecutionState::Cancelled);
    assert!(view.agents.iter().all(|a| a.state.terminal()));
    assert_eq!(live.load(Ordering::SeqCst), 0);
    assert!(!source.path().join("a.txt").exists());
}
#[tokio::test]
async fn parallel_conflicts_are_not_silently_merged() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let (manager, _, _) = setup(data.path(), 2, 3, false, 10);
    manager
        .start(
            "Conflict".into(),
            source.path().into(),
            vec![],
            Some(TaskPlan {
                tasks: vec![
                    task("a", "WRITE:same.txt VALUE:A", &[]),
                    task("b", "WRITE:same.txt VALUE:B", &[]),
                ],
            }),
        )
        .await
        .unwrap();
    let view = finish(&manager).await;
    assert_eq!(view.state, ExecutionState::Failed);
    assert!(view.summary.contains("Conflicting parallel edits"));
    assert!(view.changes.is_empty());
}
#[tokio::test]
async fn invalid_output_retries_are_bounded_and_dependents_are_blocked() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let (manager, live, _) = setup(data.path(), 1, 3, true, 5);
    manager
        .start(
            "Failure".into(),
            source.path().into(),
            vec![],
            Some(TaskPlan {
                tasks: vec![
                    task("bad", "inspect", &[]),
                    task("dependent", "inspect", &["bad"]),
                ],
            }),
        )
        .await
        .unwrap();
    let view = finish(&manager).await;
    assert_eq!(view.agents[0].state, ExecutionState::Failed);
    assert_eq!(view.agents[1].state, ExecutionState::Blocked);
    assert_eq!(live.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn live_worker_setting_is_bounded_and_retired_workers_do_not_take_new_tasks() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let (manager, _, peak) = setup(data.path(), 2, 50, false, 80);
    let tasks = (0..8)
        .map(|i| task(&format!("t{i}"), "inspect", &[]))
        .collect();
    manager
        .start(
            "Resize".into(),
            source.path().into(),
            vec![],
            Some(TaskPlan { tasks }),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    manager.set_workers(1).await.unwrap();
    assert!(manager.set_workers(0).await.is_err());
    assert!(manager.set_workers(51).await.is_err());
    let result = finish(&manager).await;
    assert_eq!(result.state, ExecutionState::Succeeded);
    assert_eq!(result.configured, 1);
    assert!(peak.load(Ordering::SeqCst) <= 2);
}
#[tokio::test]
async fn applying_source_requires_approval_and_rechecks_concurrent_edits() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let (manager, _, _) = setup(data.path(), 1, 1, false, 1);
    manager
        .start(
            "Write".into(),
            source.path().into(),
            vec![],
            Some(TaskPlan {
                tasks: vec![task("a", "WRITE:a.txt", &[])],
            }),
        )
        .await
        .unwrap();
    finish(&manager).await;
    let other = manager.clone();
    let pending = tokio::spawn(async move { other.request_apply(CancellationToken::new()).await });
    let request = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(r) = manager.approvals.pending().await.first() {
                return r.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!source.path().join("a.txt").exists());
    std::fs::write(source.path().join("a.txt"), "user edit").unwrap();
    manager.approvals.decide(&request.id, true).await;
    assert!(pending.await.unwrap().is_err());
    assert_eq!(
        std::fs::read_to_string(source.path().join("a.txt")).unwrap(),
        "user edit"
    );
}

#[tokio::test]
async fn saved_changes_can_be_confirmed_after_restart_without_duplicate_application() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let (original, _, _) = setup(data.path(), 1, 1, false, 1);
    original.start("Write".into(), source.path().into(), vec![], Some(TaskPlan {
        tasks: vec![task("a", "WRITE:a.txt", &[])],
    })).await.unwrap();
    let completed = finish(&original).await;
    let (manager, live, _) = setup(data.path(), 1, 1, false, 1);
    for allowed in [false, true] {
        let other = manager.clone();
        let id = completed.id.clone();
        let applying = tokio::spawn(async move { other.request_apply_run(&id, CancellationToken::new()).await });
        let request = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(request) = manager.approvals.pending().await.first() { break request.clone(); }
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        assert!(!source.path().join("a.txt").exists());
        assert_eq!(request.action["run_id"], completed.id);
        // A second click must fail immediately, rather than queue another approval.
        assert!(tokio::time::timeout(Duration::from_millis(100), manager.request_apply_run(&completed.id, CancellationToken::new())).await.unwrap().is_err());
        assert_eq!(manager.approvals.pending().await.len(), 1);
        assert!(manager.approvals.decide(&request.id, allowed).await);
        assert_eq!(applying.await.unwrap().is_ok(), allowed);
        let saved = gemini_cdp_cli::workbench::read_run(data.path(), &completed.id).await.unwrap();
        assert_eq!(saved.applied, allowed);
    }
    assert_eq!(std::fs::read_to_string(source.path().join("a.txt")).unwrap(), "A");
    assert!(manager.request_apply_run(&completed.id, CancellationToken::new()).await.is_err());
    assert!(manager.approvals.pending().await.is_empty());
    assert_eq!(live.load(Ordering::SeqCst), 0);
}
struct DriftFactory;
struct DriftSession(usize);
#[async_trait]
impl SessionFactory for DriftFactory {
    async fn create(&self) -> anyhow::Result<Box<dyn ModelSession>> {
        Ok(Box::new(DriftSession(0)))
    }
}
#[async_trait]
impl ModelSession for DriftSession {
    async fn ask(
        &mut self,
        prompt: &str,
        _: &[Attachment],
        _: &CancellationToken,
        _: &EventBus,
        _: &str,
    ) -> Result<String, ModelError> {
        self.0 += 1;
        match self.0 {
            1 => {
                assert!(prompt.contains("Read-only stage"));
                Ok(serde_json::json!({"summary":"Wrongly initialize project","done":false,"actions":[{"tool":"write_file","path":"should-not-exist.rs","content":"bad"}]}).to_string())
            }
            2 => {
                assert!(prompt.contains("read-only stage"));
                Ok("Here is an ordinary tutorial. It is not an executable tool response.".into())
            }
            3=>{assert!(prompt.contains("Format correction"));Ok(serde_json::json!({"summary":"Inspect the actual directory","done":false,"actions":[{"tool":"list_directory","path":"."}]}).to_string())},
            _ => {
                Ok(serde_json::json!({"summary":"Inspection complete; workspace empty","done":true,"actions":[]}).to_string())
            }
        }
    }
}
#[tokio::test]
async fn analysis_cannot_write_and_protocol_drift_is_repaired_without_losing_scope() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let prefs = AgentPreferences {
        workers: 1,
        max_sessions: 1,
        min_request_interval_secs: 0,
        ..Default::default()
    };
    let sessions = SessionManager::with_factory(Arc::new(DriftFactory), &prefs);
    let manager = AgentManager::new(sessions, prefs, data.path().into(), EventBus::default());
    manager
        .start(
            "Create a Rust application".into(),
            source.path().into(),
            vec![],
            Some(TaskPlan {
                tasks: vec![task(
                    "architecture",
                    "Inspect the empty directory; report findings only",
                    &[],
                )],
            }),
        )
        .await
        .unwrap();
    let run = finish(&manager).await;
    assert_eq!(run.state, ExecutionState::Succeeded);
    assert!(run.changes.is_empty());
    assert!(!source.path().join("should-not-exist.rs").exists());
    assert!(run.agents[0]
        .history
        .iter()
        .any(|m| m.role == "tool" && m.content.contains("read-only")));
    assert!(run.agents[0].input_chars > 0 && run.agents[0].output_chars > 0);
}
struct ClaimFactory;
struct ClaimSession;
#[async_trait]
impl SessionFactory for ClaimFactory {
    async fn create(&self) -> anyhow::Result<Box<dyn ModelSession>> {
        Ok(Box::new(ClaimSession))
    }
}
#[async_trait]
impl ModelSession for ClaimSession {
    async fn ask(
        &mut self,
        _: &str,
        _: &[Attachment],
        _: &CancellationToken,
        _: &EventBus,
        _: &str,
    ) -> Result<String, ModelError> {
        Ok(serde_json::json!({"summary":"I read and changed everything without using tools","done":true,"actions":[]}).to_string())
    }
}
#[tokio::test]
async fn unsupported_model_claims_are_not_reported_as_successful_work() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let prefs = AgentPreferences {
        workers: 1,
        max_sessions: 1,
        min_request_interval_secs: 0,
        max_retries: 1,
        ..Default::default()
    };
    let sessions = SessionManager::with_factory(Arc::new(ClaimFactory), &prefs);
    let manager = AgentManager::new(sessions, prefs, data.path().into(), EventBus::default());
    manager
        .start(
            "Read files".into(),
            source.path().into(),
            vec![],
            Some(TaskPlan {
                tasks: vec![task("check", "Read actual files", &[])],
            }),
        )
        .await
        .unwrap();
    let result = finish(&manager).await;
    assert_eq!(result.state, ExecutionState::Failed);
    assert!(result.agents[0]
        .error
        .as_ref()
        .unwrap()
        .contains("Unverified completion"));
    assert!(result.agents[0].output.is_none());
}
