use anyhow::{Context, Result};
use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub text: String,
    #[serde(default)]
    pub interrupted: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chat {
    pub id: String,
    pub title: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub url: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub blur: bool,
    pub font_size: u8,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            blur: true,
            font_size: 15,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    pub chats: Vec<Chat>,
    pub active_id: String,
    pub settings: Settings,
}

impl History {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read(path)?;
        serde_json::from_slice(&data).context(
            "Не удалось прочитать историю. Исходный файл сохранён; проверьте data/history.json",
        )
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("json.tmp");
        let data = serde_json::to_vec_pretty(self)?;
        {
            use std::io::Write;
            let mut file = fs::File::create(&temp)?;
            file.write_all(&data)?;
            file.sync_all()?;
        }
        fs::rename(temp, path).context("Не удалось сохранить историю")
    }
    pub fn new_chat(&mut self) {
        // Reuse a blank draft rather than filling the sidebar with empty chats.
        if let Some(chat) = self.chats.iter().find(|chat| chat.messages.is_empty()) {
            self.active_id = chat.id.clone();
            return;
        }
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_string();
        self.chats.insert(
            0,
            Chat {
                id: id.clone(),
                title: "Новый диалог".into(),
                messages: vec![],
                url: String::new(),
            },
        );
        self.active_id = id;
    }
    pub fn active(&self) -> Option<&Chat> {
        self.chats.iter().find(|chat| chat.id == self.active_id)
    }
    pub fn active_mut(&mut self) -> Option<&mut Chat> {
        self.chats.iter_mut().find(|chat| chat.id == self.active_id)
    }
}

/// Model text is untrusted. Escape raw HTML and disallow executable link schemes.
pub fn render_markdown(text: &str) -> String {
    let events = Parser::new_ext(text, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH).map(
        |event| match event {
            Event::Html(value) | Event::InlineHtml(value) => Event::Text(value),
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                let allowed = dest_url.starts_with("https://")
                    || dest_url.starts_with("http://")
                    || dest_url.starts_with('#');
                Event::Start(Tag::Link {
                    link_type,
                    dest_url: if allowed {
                        dest_url
                    } else {
                        CowStr::Borrowed("#")
                    },
                    title,
                    id,
                })
            }
            // Remote images are neither fetched nor treated as HTML.
            Event::Start(Tag::Image {
                link_type,
                dest_url: _,
                title,
                id,
            }) => Event::Start(Tag::Image {
                link_type,
                dest_url: CowStr::Borrowed(""),
                title,
                id,
            }),
            other => other,
        },
    );
    let mut output = String::new();
    html::push_html(&mut output, events);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_html_and_script_links_cannot_execute() {
        let html = render_markdown("<script>alert(1)</script>\n\n[bad](javascript:alert%281%29)\n\n[good](https://example.com)");
        assert!(!html.contains("<script>"));
        assert!(!html.contains("href=\"javascript:"));
        assert!(html.contains("href=\"https://example.com\""));
    }
    #[test]
    fn history_keeps_each_conversation_separate() {
        let mut history = History::default();
        history.new_chat();
        let first = history.active_id.clone();
        history.active_mut().unwrap().messages.push(Message {
            role: "user".into(),
            text: "hello".into(),
            interrupted: false,
        });
        history.new_chat();
        assert_ne!(first, history.active_id);
        let second = history.active_id.clone();
        history.new_chat();
        assert_eq!(second, history.active_id);
        let restored: History =
            serde_json::from_str(&serde_json::to_string(&history).unwrap()).unwrap();
        assert_eq!(restored.chats.len(), 2);
        assert_eq!(restored.chats[1].messages[0].text, "hello");
    }
}
