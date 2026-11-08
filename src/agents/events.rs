use super::model::*;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AgentEvent {
    Activity { agent_id: String, role: String, content: String },
    Plan(TaskPlan),
    Run(RunView),
    Progress {
        agent_id: String,
        message: String,
    },
    Approval(crate::tools::ApprovalRequest),
    ApprovalResolved {
        id: String,
        allowed: bool,
    },
    Attachment {
        agent_id: String,
        name: String,
        progress: u8,
    },
    Error(String),
}
#[derive(Clone)]
pub struct EventBus {
    tx: tokio::sync::broadcast::Sender<AgentEvent>,
}
impl Default for EventBus {
    fn default() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(32);
        Self { tx }
    }
}
impl EventBus {
    pub fn publish(&self, event: AgentEvent) {
        let _ = self.tx.send(event);
    }
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<AgentEvent> {
        self.tx.subscribe()
    }
}
