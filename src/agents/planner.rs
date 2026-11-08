use super::model::*;
use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
pub struct TaskPlanner;
impl TaskPlanner {
    /// A bounded, dependency-aware fallback; callers can also supply a validated model-generated DAG.
    pub fn plan(goal: &str) -> TaskPlan {
        let mut tasks=vec![TaskSpec{id:"architecture".into(),title:"Architecture and constraints".into(),instruction:format!("Analyze the project and this request: {goal}. Read relevant code. Identify exact files and integration risks. This is read-only analysis, not implementation. Never initialize a project or run commands. If the folder is empty, report that fact and finish with done=true and actions=[]."),dependencies:vec![],priority:20},TaskSpec{id:"test-design".into(),title:"Tests and failure cases".into(),instruction:format!("Independently inspect the project for this request: {goal}. Identify meaningful regression tests and performance risks. This is read-only analysis, not implementation. Never initialize a project or run commands. If the folder is empty, report that fact and finish with done=true and actions=[]."),dependencies:vec![],priority:10}];
        tasks.push(TaskSpec{id:"implementation".into(),title:"Implement changes".into(),instruction:format!("Implement the request using the dependency findings: {goal}. Read before editing. Edit only this isolated working copy. Run relevant build/tests through tools. Report actual results, not assumed success."),dependencies:vec!["architecture".into(),"test-design".into()],priority:5});
        tasks.push(TaskSpec{id:"verification".into(),title:"Verify integration".into(),instruction:"Review the combined implementation, inspect diffs, and run meaningful tests. Fix defects in the isolated copy if needed. Clearly report failures and unverified behavior.".into(),dependencies:vec!["implementation".into()],priority:0});
        TaskPlan { tasks }
    }
    pub fn parse(raw: &str) -> Result<TaskPlan> {
        if raw.len() > 512 * 1024 {
            bail!("Plan exceeds 512 KiB");
        }
        let plan: TaskPlan = super::protocol::decode(raw)?;
        TaskScheduler::validate(&plan)?;
        Ok(plan)
    }
}
pub struct TaskScheduler;
impl TaskScheduler {
    pub fn validate(plan: &TaskPlan) -> Result<()> {
        if plan.tasks.is_empty() || plan.tasks.len() > 100 {
            bail!("A plan must contain 1–100 tasks");
        }
        let mut indegree = HashMap::new();
        let mut edges: HashMap<&str, Vec<&str>> = HashMap::new();
        for task in &plan.tasks {
            if task.id.is_empty()
                || task.id.len() > 80
                || !task
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                || indegree
                    .insert(task.id.as_str(), task.dependencies.len())
                    .is_some()
            {
                bail!("Invalid or duplicate task ID");
            }
            if task.title.len() > 256 || task.instruction.len() > 32000 {
                bail!("Task instruction too large");
            }
        }
        for task in &plan.tasks {
            let mut seen = HashSet::new();
            for dep in &task.dependencies {
                if !indegree.contains_key(dep.as_str()) || !seen.insert(dep) {
                    bail!("Unknown or duplicate dependency: {dep}");
                }
                edges.entry(dep).or_default().push(&task.id);
            }
        }
        let mut ready: Vec<_> = indegree
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut visited = 0;
        while let Some(id) = ready.pop() {
            visited += 1;
            for next in edges.get(id).into_iter().flatten() {
                let n = indegree.get_mut(next).unwrap();
                *n -= 1;
                if *n == 0 {
                    ready.push(next);
                }
            }
        }
        if visited != plan.tasks.len() {
            bail!("Dependency cycle detected; plan rejected");
        }
        Ok(())
    }
    pub fn ready(agents: &[AgentView]) -> Vec<usize> {
        let mut ready: Vec<usize> = agents
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                matches!(
                    a.state,
                    ExecutionState::Queued | ExecutionState::WaitingCapacity
                ) && a.task.dependencies.iter().all(|d| {
                    agents
                        .iter()
                        .any(|p| p.task.id == *d && p.state == ExecutionState::Succeeded)
                })
            })
            .map(|(i, _)| i)
            .collect();
        ready.sort_by_key(|i| std::cmp::Reverse(agents[*i].task.priority));
        ready
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_cycles_and_missing_dependencies() {
        let mut p = TaskPlanner::plan("test");
        p.tasks[0].dependencies.push("verification".into());
        assert!(TaskScheduler::validate(&p).is_err());
        p.tasks[0].dependencies = vec!["unknown".into()];
        assert!(TaskScheduler::validate(&p).is_err());
    }
    #[test]
    fn independent_tasks_are_ready_first() {
        let p = TaskPlanner::plan("refactor");
        TaskScheduler::validate(&p).unwrap();
        let a: Vec<_> = p
            .tasks
            .into_iter()
            .map(|task| AgentView {
                input_chars: 0,
                output_chars: 0,
                id: id("a"),
                task,
                state: ExecutionState::Queued,
                progress: String::new(),
                steps: 0,
                attempt: 0,
                output: None,
                error: None,
                history: vec![],
            })
            .collect();
        assert_eq!(TaskScheduler::ready(&a), vec![0, 1]);
    }
}
