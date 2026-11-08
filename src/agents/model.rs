use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
pub fn id(prefix: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "{prefix}-{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentPreferences {
    pub workers: usize,
    pub max_sessions: usize,
    pub min_request_interval_secs: u64,
    pub rate_limit_pause_secs: u64,
    pub task_timeout_secs: u64,
    pub max_steps: usize,
    pub max_retries: usize,
    pub max_attachment_bytes: u64,
}
impl Default for AgentPreferences {
    fn default() -> Self {
        Self {
            workers: 3,
            max_sessions: 2,
            min_request_interval_secs: 5,
            rate_limit_pause_secs: 60,
            task_timeout_secs: 900,
            max_steps: 16,
            max_retries: 2,
            max_attachment_bytes: 20 * 1024 * 1024,
        }
    }
}
impl AgentPreferences {
    pub fn normalize(&mut self) {
        self.workers = self.workers.clamp(1, 50);
        self.max_sessions = self.max_sessions.clamp(1, 8);
        self.max_steps = self.max_steps.clamp(1, 64);
        self.max_retries = self.max_retries.min(5);
        self.task_timeout_secs = self.task_timeout_secs.clamp(30, 7200);
        self.min_request_interval_secs = self.min_request_interval_secs.max(1);
        self.rate_limit_pause_secs = self.rate_limit_pause_secs.max(30);
        self.max_attachment_bytes = self.max_attachment_bytes.clamp(1024, 50 * 1024 * 1024);
    }
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let mut value = if path.exists() {
            serde_json::from_slice(&std::fs::read(path)?)?
        } else {
            Self::default()
        };
        Self::normalize(&mut value);
        Ok(value)
    }
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Queued,
    WaitingCapacity,
    Running,
    WaitingApproval,
    RateLimited,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
    Blocked,
}
impl ExecutionState {
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Blocked | Self::Paused
        )
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    pub id: String,
    pub title: String,
    pub instruction: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub priority: i32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlan {
    pub tasks: Vec<TaskSpec>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOutput {
    pub summary: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub verification: Vec<crate::tools::ToolResult>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentView {
    pub id: String,
    pub task: TaskSpec,
    pub state: ExecutionState,
    pub progress: String,
    pub steps: usize,
    pub attempt: usize,
    pub output: Option<AgentOutput>,
    pub error: Option<String>,
    #[serde(default)]
    pub history: Vec<ConversationMessage>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub role: String,
    pub content: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunView {
    pub id: String,
    pub goal: String,
    pub workspace: PathBuf,
    pub configured: usize,
    pub active: usize,
    pub waiting_capacity: usize,
    pub state: ExecutionState,
    pub agents: Vec<AgentView>,
    pub summary: String,
    pub changes: Vec<crate::tools::FileChange>,
}
impl RunView {
    /// Keep UI broadcasts small; full bounded transcripts remain in result.json.
    pub fn event_snapshot(&self) -> Self {
        let mut value = Self {
            id: self.id.clone(),
            goal: self.goal.clone(),
            workspace: self.workspace.clone(),
            configured: self.configured,
            active: self.active,
            waiting_capacity: self.waiting_capacity,
            state: self.state.clone(),
            agents: vec![],
            summary: self.summary.clone(),
            changes: self.changes.clone(),
        };
        value.agents = self
            .agents
            .iter()
            .map(|a| {
                let mut output = a.output.clone();
                if let Some(o) = output.as_mut() {
                    o.verification = o
                        .verification
                        .iter()
                        .rev()
                        .take(16)
                        .cloned()
                        .map(|mut r| {
                            r.output = crate::tools::truncate(&r.output, 2000);
                            r
                        })
                        .collect();
                }
                AgentView {
                    id: a.id.clone(),
                    task: a.task.clone(),
                    state: a.state.clone(),
                    progress: a.progress.clone(),
                    steps: a.steps,
                    attempt: a.attempt,
                    output,
                    error: a.error.clone(),
                    history: vec![],
                }
            })
            .collect();
        value
    }
}
