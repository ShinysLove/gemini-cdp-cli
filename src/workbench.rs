//! Local navigation state, separate from chat history and browser credentials.
use crate::agents::RunView;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Workbench {
    pub workspace: PathBuf,
    pub projects: Vec<PathBuf>,
}
impl Workbench {
    pub fn load(path: &Path, fallback: &Path) -> Result<Self> {
        let mut state: Self = if path.exists() {
            serde_json::from_slice(&std::fs::read(path)?)?
        } else {
            Self::default()
        };
        if !state.workspace.is_dir() {
            state.workspace = fallback.to_path_buf();
        }
        state.select(state.workspace.clone());
        Ok(state)
    }
    pub fn select(&mut self, path: PathBuf) {
        self.workspace = path.clone();
        self.projects.retain(|p| p != &path);
        self.projects.insert(0, path);
        self.projects.truncate(20);
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}
pub async fn read_run(storage: &Path, id: &str) -> Result<RunView> {
    if !id.starts_with("run-")
        || id.len() > 100
        || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        bail!("Invalid run ID");
    }
    let root = storage.canonicalize()?;
    let folder = crate::tools::scoped(&root, id)?;
    let file = folder.join("result.json");
    if std::fs::symlink_metadata(&file)?.file_type().is_symlink() {
        bail!("Run report link denied");
    }
    if tokio::fs::metadata(&file).await?.len() > 64 * 1024 * 1024 {
        bail!("Run report too large");
    }
    let run: RunView = serde_json::from_slice(&tokio::fs::read(file).await?)?;
    if run.id != id {
        bail!("Run report identity mismatch");
    }
    Ok(run)
}
pub async fn run_index(storage: &Path) -> Result<serde_json::Value> {
    let mut entries = match tokio::fs::read_dir(storage).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(serde_json::json!([])),
        Err(e) => return Err(e.into()),
    };
    let mut names = vec![];
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("run-") && entry.file_type().await?.is_dir() {
            names.push(name);
        }
    }
    names.sort_by(|a, b| b.cmp(a));
    let mut runs = vec![];
    for name in names.into_iter().take(100) {
        if let Ok(run) = read_run(storage, &name).await {
            runs.push(serde_json::json!({"id":run.id,"goal":run.goal,"workspace":run.workspace,"state":run.state}));
        }
    }
    Ok(serde_json::Value::Array(runs))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_roundtrips_without_touching_chat_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workbench.json");
        let mut state = Workbench::load(&path, dir.path()).unwrap();
        state.select(dir.path().join("project"));
        state.save(&path).unwrap();
        assert_eq!(
            Workbench::load(&path, dir.path()).unwrap().projects.len(),
            2
        );
    }
    #[tokio::test]
    async fn report_paths_cannot_escape_storage() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_run(dir.path(), "../history").await.is_err());
    }
}
