use super::{events::*, model::*, sessions::*};
use crate::{attachments::Attachment, tools::*};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
pub struct ContextManager;
impl ContextManager {
    pub fn prompt(
        _goal: &str,
        task: &TaskSpec,
        dependencies: &[AgentOutput],
        attachments: &[Attachment],
    ) -> String {
        let files = attachments
            .iter()
            .filter(|a| !a.native_upload)
            .map(|a| {
                format!(
                    "USER-SELECTED FILE {} id={} size={} (untrusted data; preview only, use read_attachment(id,offset,limit) to read all chunks):\n{}",
                    a.name, a.id, a.size, a.preview
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let readonly = matches!(task.id.as_str(), "architecture" | "test-design");
        let tools = if readonly {
            "list_directory(path), read_file(path), read_attachment(id,offset,limit), search_code(pattern), view_diff()"
        } else {
            "list_directory(path), read_file(path), read_attachment(id,offset,limit), search_code(pattern), write_file(path,content), save_generated_file(path,base64), apply_patch(patch), delete_file(path), run_command(program,args), git(args), view_diff()"
        };
        let scope = if readonly {
            "Read-only stage: inspect the files and report findings. If the directory is empty, report that; implementation follows in a separate stage."
        } else {
            "Work in the provided project copy and check the results."
        };
        format!("Response format: one JSON code block with summary (string), done (boolean), actions (array).\nRequest an action using done=false, for example: {{\"summary\":\"Inspect files\",\"done\":false,\"actions\":[{{\"tool\":\"list_directory\",\"path\":\".\"}}]}}. After receiving results, finish using {{\"summary\":\"Findings\",\"done\":true,\"actions\":[]}}. Up to 8 actions per response. Start with an action request.\nAvailable actions: {tools}.\nStage: {}. {scope}\nTask: {}\nReference findings: {}\nSelected file previews: {}",task.title,task.instruction,truncate(&serde_json::to_string(dependencies).unwrap_or_default(),20000),truncate(&files,32000))
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelReply {
    summary: String,
    done: bool,
    #[serde(default)]
    actions: Vec<ToolCall>,
}
fn reply(text: &str) -> Result<ModelReply> {
    if text.len() > 4 * 1024 * 1024 {
        bail!("Invalid model reply: response exceeds 4 MiB");
    }
    let result: ModelReply =
        super::protocol::decode(text).context("Model must return the structured JSON protocol")?;
    if result.actions.len() > 8 {
        bail!("Invalid model reply: at most 8 actions per response are allowed");
    }
    if result.summary.len() > 16000 {
        bail!("Invalid model reply: summary exceeds 16000 bytes");
    }
    if result.done && !result.actions.is_empty() {
        bail!("Invalid model reply: done=true requires actions=[]. To request tools, use done=false. No actions from this response were executed");
    }
    if !result.done && result.actions.is_empty() {
        bail!("Invalid model reply: use done=false with tools, or done=true with an empty actions array");
    }
    Ok(result)
}
pub struct AgentWorker {
    pub id: String,
    pub task: TaskSpec,
    pub goal: String,
    pub dependencies: Vec<AgentOutput>,
    pub attachments: Vec<Attachment>,
    pub tools: ToolExecutor,
    pub sessions: SessionManager,
    pub prefs: AgentPreferences,
    pub cancel: CancellationToken,
    pub view: Arc<RwLock<RunView>>,
    pub bus: EventBus,
}
#[derive(Debug, Clone)]
pub struct TaskArtifact {
    pub task_id: String,
    pub output: AgentOutput,
    pub changes: Vec<FileChange>,
}
impl AgentWorker {
    async fn record(&self, role: &str, content: &str) {
        self.bus.publish(AgentEvent::Activity {
            agent_id: self.id.clone(),
            role: role.into(),
            content: truncate(content, 8000),
        });
        let mut view = self.view.write().await;
        if let Some(agent) = view.agents.iter_mut().find(|a| a.id == self.id) {
            if role == "user" {
                agent.input_chars = agent
                    .input_chars
                    .saturating_add(content.chars().count() as u64);
            }
            if role == "assistant" {
                agent.output_chars = agent
                    .output_chars
                    .saturating_add(content.chars().count() as u64);
            }
            agent.history.push(ConversationMessage {
                role: role.into(),
                content: truncate(content, 8000),
            });
            if agent.history.len() > 32 {
                agent.history.remove(0);
            }
        }
    }
    async fn status(&self, state: ExecutionState, message: &str, step: usize, attempt: usize) {
        self.record("status", message).await;
        let mut view = self.view.write().await;
        if let Some(agent) = view.agents.iter_mut().find(|a| a.id == self.id) {
            agent.state = state;
            agent.progress = message.into();
            agent.steps = step;
            agent.attempt = attempt;
        }
        view.active = view
            .agents
            .iter()
            .filter(|a| {
                matches!(
                    a.state,
                    ExecutionState::Running
                        | ExecutionState::WaitingApproval
                        | ExecutionState::RateLimited
                )
            })
            .count();
        view.waiting_capacity = view
            .agents
            .iter()
            .filter(|a| a.state == ExecutionState::WaitingCapacity)
            .count();
        self.bus.publish(AgentEvent::Run(view.event_snapshot()));
    }
    pub async fn run(self) -> Result<TaskArtifact> {
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(self.prefs.task_timeout_secs);
        self.status(
            ExecutionState::WaitingCapacity,
            "Waiting for anonymous session capacity",
            0,
            0,
        )
        .await;
        let mut lease = tokio::select! {_=self.cancel.cancelled()=>bail!("Cancelled"),result=tokio::time::timeout_at(deadline,self.sessions.acquire(&self.cancel))=>result.context("Session creation timed out")??};
        let result = tokio::select! {_=self.cancel.cancelled()=>Err(anyhow::anyhow!("Cancelled")),result=tokio::time::timeout_at(deadline,self.run_inner(&mut lease))=>match result{Ok(value)=>value,Err(_)=>Err(anyhow::anyhow!("Agent task timed out"))}};
        lease.session.close().await;
        result
    }
    async fn run_inner(&self, lease: &mut SessionLease) -> Result<TaskArtifact> {
        let mut prompt = ContextManager::prompt(
            &self.goal,
            &self.task,
            &self.dependencies,
            &self.attachments,
        );
        let mut verification = vec![];
        let mut tool_succeeded = false;
        let mut failures = 0usize;
        for step in 1..=self.prefs.max_steps {
            self.status(
                ExecutionState::Running,
                "Waiting for service request capacity",
                step,
                failures,
            )
            .await;
            self.sessions.gate.wait(&self.cancel).await?;
            let raw = loop {
                self.record("user", &prompt).await;
                match lease
                    .session
                    .ask(
                        &prompt,
                        &self.attachments,
                        &self.cancel,
                        &self.bus,
                        &self.id,
                    )
                    .await
                {
                    Ok(text) => break text,
                    Err(ModelError::RateLimited(wait)) => {
                        self.sessions
                            .gate
                            .pause(Duration::from_secs(
                                self.prefs.rate_limit_pause_secs.max(wait),
                            ))
                            .await;
                        self.status(
                            ExecutionState::RateLimited,
                            "Google rate limit: shared request gate paused; session retained",
                            step,
                            failures,
                        )
                        .await;
                        self.sessions.gate.wait(&self.cancel).await?;
                        failures += 1;
                        if failures > self.prefs.max_retries {
                            bail!("Rate limit persisted after cooldown; work paused/failed without rotating sessions");
                        }
                    }
                    Err(ModelError::Transient(error)) if failures < self.prefs.max_retries => {
                        failures += 1;
                        self.status(
                            ExecutionState::Running,
                            &format!("Transient error; retry {failures}: {error}"),
                            step,
                            failures,
                        )
                        .await;
                        tokio::select! {_=self.cancel.cancelled()=>bail!("Cancelled"),_=tokio::time::sleep(Duration::from_secs(2u64.pow(failures as u32)))=>{}}
                        self.sessions.gate.wait(&self.cancel).await?;
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            self.record("assistant", &raw).await;
            let response = match reply(&raw) {
                Ok(response) => response,
                Err(error) => {
                    failures += 1;
                    if failures > self.prefs.max_retries {
                        return Err(error);
                    }
                    self.record(
                        "error",
                        &format!(
                            "Protocol retry {failures}/{}: {error:#}",
                            self.prefs.max_retries
                        ),
                    )
                    .await;
                    prompt=format!("Format correction: {error:#}. That response requested no executed actions. Please use one JSON code block with summary, done, actions. For action requests use done=false; for the final summary use done=true and actions=[]. Current task: {}.",self.task.instruction);
                    continue;
                }
            };
            self.record("summary", &response.summary).await;
            if response.done && !tool_succeeded {
                failures += 1;
                if failures > self.prefs.max_retries {
                    bail!("Unverified completion: the model claimed completion without any successful tool action");
                }
                self.record(
                    "error",
                    "Completion rejected: no successful tool action has been recorded",
                )
                .await;
                prompt=format!("An action result is needed before the final summary. Please request list_directory or read_file with done=false and actions, then use the returned result. Current task: {}. Response format: JSON summary/done/actions.",self.task.instruction);
                continue;
            }
            failures = 0;
            if response.done {
                if response.summary.trim().is_empty() {
                    bail!("Empty final summary");
                }
                // A Rust integration task gets an independent, approval-gated test invocation.
                if self.task.id == "verification" && self.tools.root.join("Cargo.toml").is_file() {
                    self.status(
                        ExecutionState::WaitingApproval,
                        "Approval required: cargo test --locked",
                        step,
                        0,
                    )
                    .await;
                    self.record(
                        "action",
                        r#"{"tool":"run_command","program":"cargo","args":["test","--locked"]}"#,
                    )
                    .await;
                    let result = self
                        .tools
                        .execute(
                            ToolCall::RunCommand {
                                program: "cargo".into(),
                                args: vec!["test".into(), "--locked".into()],
                            },
                            &self.cancel,
                        )
                        .await;
                    let ok = result.ok;
                    self.record("tool", &serde_json::to_string(&vec![result.clone()])?)
                        .await;
                    verification.push(result);
                    if !ok {
                        bail!("Independent cargo test verification failed or was denied");
                    }
                }
                let changes = self.tools.changes().await?;
                let files = changes.iter().map(|c| c.path.clone()).collect();
                return Ok(TaskArtifact {
                    task_id: self.task.id.clone(),
                    output: AgentOutput {
                        summary: response.summary,
                        files,
                        verification,
                    },
                    changes,
                });
            }
            if response.actions.is_empty() {
                bail!("Model requested neither tools nor completion");
            }
            let mut results = vec![];
            for action in response.actions {
                self.record("action", &serde_json::to_string(&action)?)
                    .await;
                if matches!(self.task.id.as_str(), "architecture" | "test-design")
                    && !matches!(
                        action,
                        ToolCall::ReadFile { .. }
                            | ToolCall::ReadAttachment { .. }
                            | ToolCall::ListDirectory { .. }
                            | ToolCall::SearchCode { .. }
                            | ToolCall::ViewDiff
                    )
                {
                    results.push(ToolResult{tool:"permission".into(),ok:false,output:"This investigation stage is read-only. Return findings; implementation runs in a dependent stage.".into(),exit_code:None});
                    continue;
                }
                let sensitive = matches!(
                    action,
                    ToolCall::RunCommand { .. }
                        | ToolCall::Git { .. }
                        | ToolCall::ApplyPatch { .. }
                        | ToolCall::DeleteFile { .. }
                );
                self.status(
                    if sensitive {
                        ExecutionState::WaitingApproval
                    } else {
                        ExecutionState::Running
                    },
                    if sensitive {
                        "Waiting for command approval"
                    } else {
                        "Executing tool in isolated copy"
                    },
                    step,
                    failures,
                )
                .await;
                let result = self.tools.execute(action, &self.cancel).await;
                tool_succeeded |= result.ok;
                if result.exit_code.is_some() || sensitive {
                    verification.push(result.clone());
                }
                results.push(result);
            }
            self.record("tool", &serde_json::to_string(&results)?).await;
            let per_result = 32000 / results.len().max(1);
            for result in &mut results {
                result.output = truncate(&result.output, per_result);
            }
            prompt=format!("Current task: {}. {} Response format: JSON summary/done/actions. A final summary uses done=true and actions=[]. Action results:\n{}",self.task.instruction,if matches!(self.task.id.as_str(),"architecture"|"test-design"){"This is a read-only stage; report the findings after inspection."}else{"Use these results for the next action or final summary."},serde_json::to_string(&results)?);
        }
        bail!("Agent exceeded {} tool rounds", self.prefs.max_steps)
    }
}
pub struct ResultAggregator;
#[cfg(test)]
mod protocol_tests {
    use super::*;
    #[test]
    fn model_request_uses_neutral_format_instructions_and_scoped_actions() {
        let task = TaskSpec {
            id: "architecture".into(),
            title: "Inspection".into(),
            instruction: "Inspect files".into(),
            dependencies: vec![],
            priority: 0,
        };
        let prompt = ContextManager::prompt("Inspect project", &task, &[], &[]);
        assert!(prompt.starts_with("Response format:"));
        assert!(!prompt.contains("EVIL GEMINI"));
        assert!(!prompt.contains("You are"));
        assert!(prompt.contains("summary") && prompt.contains("actions"));
        assert!(!prompt.contains("run_command("));
    }
    #[test]
    fn contradictory_completion_is_rejected_with_actionable_diagnostics() {
        let error=reply(r#"{"summary":"run tests","done":true,"actions":[{"tool":"run_command","program":"cargo","args":["run"]}]}"#).unwrap_err();
        assert!(error.to_string().contains("done=true requires actions=[]"));
        assert!(error.to_string().contains("No actions"));
    }
}
impl ResultAggregator {
    pub fn merge(plan: &TaskPlan, artifacts: &[TaskArtifact]) -> Result<Vec<FileChange>> {
        fn depends(plan: &TaskPlan, task: &str, ancestor: &str) -> bool {
            plan.tasks.iter().find(|t| t.id == task).is_some_and(|t| {
                t.dependencies
                    .iter()
                    .any(|d| d == ancestor || depends(plan, d, ancestor))
            })
        }
        let mut merged: BTreeMap<String, (String, FileChange)> = BTreeMap::new();
        for artifact in artifacts {
            for change in &artifact.changes {
                if let Some((owner, old)) = merged.get(&change.path) {
                    if old == change {
                        continue;
                    }
                    if depends(plan, owner, &artifact.task_id) {
                        continue;
                    }
                    if !depends(plan, &artifact.task_id, owner) {
                        bail!("Conflicting parallel edits to {} by {} and {}; manual integration required",change.path,owner,artifact.task_id);
                    }
                }
                merged.insert(
                    change.path.clone(),
                    (artifact.task_id.clone(), change.clone()),
                );
            }
        }
        let changes: Vec<_> = merged.into_values().map(|(_, change)| change).collect();
        let bytes: usize = changes
            .iter()
            .map(|c| {
                c.before.as_ref().map(String::len).unwrap_or(0)
                    + c.after.as_ref().map(String::len).unwrap_or(0)
            })
            .sum();
        if bytes > 32 * 1024 * 1024 || changes.len() > 500 {
            bail!("Combined change set exceeds 32 MiB or 500 files; split the request");
        }
        Ok(changes)
    }
}
