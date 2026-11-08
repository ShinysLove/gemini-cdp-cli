use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub preview: String,
    pub sha256: String,
    pub stored_path: PathBuf,
    pub native_upload: bool,
}
impl Attachment {
    pub async fn from_clipboard(mime: &str, data: &str, storage: PathBuf, max: u64) -> Result<Self> {
        use base64::Engine;
        let ext = match mime {
            "image/png" => "png", "image/jpeg" => "jpg", "image/webp" => "webp",
            "image/gif" => "gif", "image/bmp" => "bmp",
            _ => bail!("Unsupported clipboard media type: {mime}"),
        };
        if data.len() as u64 > max.saturating_add(2) / 3 * 4 { bail!("Clipboard image is too large"); }
        let bytes = base64::engine::general_purpose::STANDARD.decode(data).context("Invalid clipboard image")?;
        if bytes.is_empty() || bytes.len() as u64 > max { bail!("Clipboard image is empty or too large"); }
        let valid = match ext {
            "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            "jpg" => bytes.starts_with(b"\xff\xd8\xff"),
            "gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
            "webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
            "bmp" => bytes.starts_with(b"BM"),
            _ => false,
        };
        if !valid { bail!("Clipboard image header does not match its type"); }
        let id = crate::agents::model::id("clipboard");
        let name = format!("{id}.{ext}");
        let dir = storage.join(&id);
        tokio::fs::create_dir_all(&dir).await?;
        let stored_path = dir.join(&name);
        tokio::fs::write(&stored_path, &bytes).await?;
        Ok(Self { id, name, size: bytes.len() as u64, mime: mime.into(),
            preview: format!("{mime} · {} bytes", bytes.len()), sha256: crate::tools::digest(&bytes),
            stored_path, native_upload: true })
    }
    /// Called only on paths supplied by the user, never from a model tool call.
    pub async fn prepare(path: PathBuf, storage: PathBuf, max: u64) -> Result<Self> {
        tokio::task::spawn_blocking(move || {
            let path = path.canonicalize().context("Attachment does not exist")?;
            let meta = std::fs::metadata(&path)?;
            if !meta.is_file() || meta.len() == 0 || meta.len() > max {
                bail!(
                    "Attachment must be a nonempty file no larger than {} MiB",
                    max / 1024 / 1024
                );
            }
            let bytes = std::fs::read(&path)?;
            if bytes.len() as u64 > max {
                bail!("Attachment changed size while reading");
            }
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let ext = path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            let (mime, native) = match ext.as_str() {
                "pdf" => ("application/pdf", true),
                "png" => ("image/png", true),
                "jpg" | "jpeg" => ("image/jpeg", true),
                "webp" => ("image/webp", true),
                "gif" => ("image/gif", true),
                "bmp" => ("image/bmp", true),
                "docx" => (
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                    true,
                ),
                "txt" | "md" | "rs" | "js" | "ts" | "tsx" | "jsx" | "json" | "toml" | "yaml"
                | "yml" | "py" | "java" | "c" | "h" | "cpp" | "html" | "css" | "csv" | "xml"
                | "log" | "sh" | "sql" => ("text/plain", false),
                _ => bail!("Unsupported attachment type: .{ext}"),
            };
            if ext == "pdf" && !bytes.starts_with(b"%PDF-") {
                bail!("Invalid PDF header");
            }
            let preview = if native {
                format!("{} · {} bytes · native upload required", mime, bytes.len())
            } else {
                crate::tools::truncate(
                    std::str::from_utf8(&bytes).context("Text attachment is not UTF-8")?,
                    12000,
                )
            };
            let id = crate::agents::model::id("file");
            std::fs::create_dir_all(&storage)?;
            let dir = storage.join(&id);
            std::fs::create_dir(&dir)?;
            let stored_path = dir.join(&name);
            std::fs::write(&stored_path, &bytes)?;
            Ok(Self {
                id,
                name,
                size: bytes.len() as u64,
                mime: mime.into(),
                preview,
                sha256: crate::tools::digest(&bytes),
                stored_path,
                native_upload: native,
            })
        })
        .await?
    }
    pub async fn verify(&self) -> Result<()> {
        let bytes = tokio::fs::read(&self.stored_path).await?;
        if crate::tools::digest(&bytes) != self.sha256 {
            bail!("Attachment changed after selection: {}", self.name);
        }
        Ok(())
    }
    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }
    pub async fn image_data_url(&self) -> Result<Option<String>> {
        use base64::Engine;
        if !self.is_image() || self.size > 20 * 1024 * 1024 {
            return Ok(None);
        }
        self.verify().await?;
        Ok(Some(format!(
            "data:{};base64,{}",
            self.mime,
            base64::engine::general_purpose::STANDARD
                .encode(tokio::fs::read(&self.stored_path).await?)
        )))
    }
}
pub fn parse_paths(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            Path::new(
                s.trim_start_matches('&')
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\''),
            )
            .to_path_buf()
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn clipboard_images_are_snapshotted_and_validated() {
        use base64::Engine;
        let dir = tempfile::tempdir().unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nimage");
        let file = Attachment::from_clipboard("image/png", &data, dir.path().into(), 100).await.unwrap();
        assert!(file.native_upload);
        assert!(file.name.ends_with(".png"));
        file.verify().await.unwrap();
        assert!(file.image_data_url().await.unwrap().unwrap().ends_with(&data));
        assert!(Attachment::from_clipboard("image/png", &data, dir.path().into(), 5).await.is_err());
        assert!(Attachment::from_clipboard("image/jpeg", &data, dir.path().into(), 100).await.is_err());
        for (mime, data) in [("image/png", ""), ("image/png", "bad!"), ("video/mp4", "eA==")] {
            assert!(Attachment::from_clipboard(mime, data, dir.path().into(), 100).await.is_err());
        }
    }
    #[tokio::test]
    async fn photos_are_native_uploads_with_verified_previews() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        let bytes = b"\x89PNG\r\n\x1a\nfixture";
        std::fs::write(&path, bytes).unwrap();
        let file = Attachment::prepare(path, dir.path().join("uploads"), 1024).await.unwrap();
        assert!(file.native_upload);
        assert!(file.is_image());
        assert!(file.image_data_url().await.unwrap().unwrap().starts_with("data:image/png;base64,"));
        std::fs::write(&file.stored_path, b"changed").unwrap();
        assert!(file.image_data_url().await.is_err());
    }
    #[tokio::test]
    async fn validates_and_snapshots_user_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "fn main() {}").unwrap();
        let a = Attachment::prepare(path.clone(), dir.path().join("uploads"), 1024)
            .await
            .unwrap();
        std::fs::write(path, "changed").unwrap();
        a.verify().await.unwrap();
        assert!(a.preview.contains("fn main"));
    }
    #[tokio::test]
    async fn rejects_oversize_and_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("file.exe");
        std::fs::write(&p, [1; 10]).unwrap();
        assert!(Attachment::prepare(p.clone(), dir.path().join("out"), 5)
            .await
            .is_err());
        assert!(Attachment::prepare(p, dir.path().join("out"), 100)
            .await
            .is_err());
    }
}
