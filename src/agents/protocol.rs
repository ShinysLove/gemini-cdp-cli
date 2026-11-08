//! Decode the Markdown representation emitted by the existing, unchanged DOM parser.
use anyhow::Result;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
pub fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T> {
    if let Ok(value) = serde_json::from_str(raw.trim()) {
        return Ok(value);
    }
    let mut plain = String::new();
    let mut code = String::new();
    let mut inside = false;
    for event in Parser::new(raw.trim()) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => inside = true,
            Event::End(TagEnd::CodeBlock) => {
                if !code.trim().is_empty() {
                    if let Ok(value) = serde_json::from_str(&code) {
                        return Ok(value);
                    }
                }
                inside = false;
                code.clear();
            }
            Event::Text(text) | Event::Code(text) => {
                if inside {
                    code.push_str(&text);
                } else {
                    plain.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if inside {
                    code.push('\n');
                } else {
                    plain.push('\n');
                }
            }
            _ => {}
        }
    }
    serde_json::from_str(&plain)
        .or_else(|_| serde_json::from_str(raw.trim()))
        .map_err(Into::into)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decodes_markdown_escaped_tool_json_without_changing_file_bytes() {
        let original = serde_json::json!({"summary":"write_code","done":false,"actions":[{"tool":"write_file","path":"a.rs","content":"fn main() { println!(\"[a_b]*\"); }\nlet path = r\"C:\\test\";\n"}]});
        let json = original.to_string();
        let escaped = json
            .chars()
            .flat_map(|c| {
                if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']') {
                    vec!['\\', c]
                } else {
                    vec![c]
                }
            })
            .collect::<String>();
        let decoded: serde_json::Value = decode(&escaped).unwrap();
        assert_eq!(decoded, original);
        let fenced = format!("```json\n{json}\n```");
        let decoded: serde_json::Value = decode(&fenced).unwrap();
        assert_eq!(decoded, original);
    }
}
