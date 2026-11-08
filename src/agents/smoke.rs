//! Explicit live check: only fixed public fixtures are sent to Google.
use super::*;
use anyhow::Result;
use std::{path::PathBuf, time::Duration};
pub async fn live(cfg: crate::browser::Config) -> Result<()> {
    let storage = std::env::temp_dir().join(model::id("evil-agent-smoke"));
    let source = storage.join("project");
    tokio::fs::create_dir_all(&source).await?;
    tokio::fs::write(
        source.join("hello.txt"),
        "PUBLIC_AGENT_FIXTURE\nTwo independent anonymous conversations must read this file.\n",
    )
    .await?;
    let prefs = AgentPreferences {
        workers: 2,
        max_sessions: 2,
        max_steps: 5,
        max_retries: 2,
        task_timeout_secs: 240,
        ..Default::default()
    };
    let bus = EventBus::default();
    let mut events = bus.subscribe();
    let manager = AgentManager::new(
        SessionManager::chrome(cfg, &prefs),
        prefs,
        storage.join("runs"),
        bus,
    );
    let task = |id: &str| TaskSpec {
        id: id.into(),
        title: format!("Anonymous read {id}"),
        instruction: if id == "one" {
            "Read hello.txt using read_file, then create one.txt using write_file with the exact contents PUBLIC_AGENT_WRITE. Finish with a summary containing PUBLIC_AGENT_FIXTURE. Do not run commands.".into()
        } else {
            "Use read_file on hello.txt, then finish with a summary containing PUBLIC_AGENT_FIXTURE. Do not write files or run commands.".into()
        },
        dependencies: vec![],
        priority: 0,
    };
    manager
        .start(
            "Verify two independent anonymous tool-using agents".into(),
            PathBuf::from(&source),
            vec![],
            Some(TaskPlan {
                tasks: vec![task("one"), task("two")],
            }),
        )
        .await?;
    let result = tokio::time::timeout(Duration::from_secs(270), async {
        loop {
            match events.recv().await {
                Ok(AgentEvent::Run(view)) => {
                    println!(
                        "{} active={} waiting={} {:?}",
                        view.id,
                        view.active,
                        view.waiting_capacity,
                        view.agents
                            .iter()
                            .map(|a| (&a.task.id, &a.state))
                            .collect::<Vec<_>>()
                    );
                    if view.state.terminal() {
                        return Ok::<_, anyhow::Error>(view);
                    }
                }
                Ok(AgentEvent::Error(error)) => eprintln!("{error}"),
                _ => {}
            }
        }
    })
    .await;
    match result {
        Ok(Ok(view)) => {
            let view = manager.snapshot().await.unwrap_or(view);
            println!("{}", serde_json::to_string_pretty(&view)?);
            anyhow::ensure!(
                view.state == ExecutionState::Succeeded,
                "Live agents did not complete; see structured errors above"
            );
            anyhow::ensure!(
                view.agents.iter().all(|a| a
                    .output
                    .as_ref()
                    .is_some_and(|o| o.summary.contains("PUBLIC_AGENT_FIXTURE"))),
                "Live agents did not verify fixture contents"
            );
        }
        other => {
            manager.cancel().await;
            anyhow::bail!("Live check timed out or failed: {other:?}");
        }
    }
    anyhow::ensure!(
        manager.snapshot().await.unwrap().agents.iter().all(|a| a
            .history
            .iter()
            .any(|m| m.role == "tool" && m.content.contains("PUBLIC_AGENT_FIXTURE"))),
        "No actual tool read recorded"
    );
    anyhow::ensure!(
        !source.join("one.txt").exists(),
        "Live agent modified source without approval"
    );
    anyhow::ensure!(
        manager
            .snapshot()
            .await
            .unwrap()
            .changes
            .iter()
            .any(|c| c.path == "one.txt"
                && c.after
                    .as_deref()
                    .is_some_and(|s| s.contains("PUBLIC_AGENT_WRITE"))),
        "Live agent did not stage a source change"
    );
    println!(
        "Live anonymous multi-agent check passed. Artifacts: {}",
        storage.display()
    );
    Ok(())
}
