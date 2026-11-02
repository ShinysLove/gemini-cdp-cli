use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const ACCENT_BLUE: Color = Color::Rgb(20, 120, 255);
const ACCENT_CYAN: Color = Color::Rgb(80, 170, 255);
const TEXT: Color = Color::Rgb(215, 220, 230);
const BORDER: Color = Color::Rgb(60, 80, 120);
const CODE_TEXT: Color = Color::Rgb(170, 215, 150);
const INLINE_CODE: Color = Color::Rgb(255, 200, 120);
const CODE_BG: Color = Color::Rgb(25, 30, 42);

fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// Переносит строку по словам под ширину `width`.
/// Строки-продолжения получают отступ `indent` пробелов (висячий отступ для списков и т.п.).
/// Слова длиннее строки режутся посимвольно.
pub fn wrap_line(spans: Vec<Span<'static>>, indent: usize, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let indent = if indent * 2 > width { 0 } else { indent };
    let chars: Vec<(char, Style)> = spans
        .iter()
        .flat_map(|s| s.content.chars().filter(|c| *c != '\r').map(move |c| (c, s.style)))
        .collect();

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<(char, Style)> = Vec::new();
    let mut cur_w = 0usize;
    let mut has_content = false;

    let mut push_line = |cur: &mut Vec<(char, Style)>, cur_w: &mut usize, has_content: &mut bool| {
        out.push(chars_to_line(cur));
        cur.clear();
        cur.extend(std::iter::repeat((' ', Style::default())).take(indent));
        *cur_w = indent;
        *has_content = false;
    };

    let mut i = 0;
    while i < chars.len() {
        let is_space = chars[i].0 == ' ';
        let mut j = i;
        while j < chars.len() && (chars[j].0 == ' ') == is_space {
            j += 1;
        }
        let tok = &chars[i..j];
        i = j;
        let tok_w: usize = tok.iter().map(|(c, _)| char_width(*c)).sum();

        if cur_w + tok_w <= width {
            cur.extend_from_slice(tok);
            cur_w += tok_w;
            has_content |= !is_space;
            continue;
        }
        if is_space {
            // Пробелы на месте переноса выбрасываются.
            if has_content {
                push_line(&mut cur, &mut cur_w, &mut has_content);
            }
            continue;
        }
        if has_content && indent + tok_w <= width {
            push_line(&mut cur, &mut cur_w, &mut has_content);
            cur.extend_from_slice(tok);
            cur_w += tok_w;
            has_content = true;
            continue;
        }
        for &(c, st) in tok {
            let w = char_width(c);
            if cur_w + w > width && has_content {
                push_line(&mut cur, &mut cur_w, &mut has_content);
            }
            cur.push((c, st));
            cur_w += w;
            has_content = true;
        }
    }
    out.push(chars_to_line(&cur));
    out
}

fn chars_to_line(chars: &[(char, Style)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut style: Option<Style> = None;
    for &(c, st) in chars {
        if style != Some(st) {
            if let Some(prev) = style {
                spans.push(Span::styled(std::mem::take(&mut buf), prev));
            }
            style = Some(st);
        }
        buf.push(c);
    }
    if let Some(prev) = style {
        spans.push(Span::styled(buf, prev));
    }
    Line::from(spans)
}

enum ListKind {
    Ordered(u64),
    Unordered,
}

struct ListState {
    kind: ListKind,
    /// Отступ маркера элементов этого списка.
    base: usize,
    /// Отступ текста текущего элемента (маркер + его ширина).
    content_indent: usize,
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    has_header: bool,
    cell: String,
}

struct Renderer {
    width: usize,
    out: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    bold: bool,
    italic: bool,
    strike: bool,
    link: bool,
    heading: Option<HeadingLevel>,
    quote_depth: usize,
    lists: Vec<ListState>,
    code: Option<(String, String)>, // (язык, накопленный код)
    table: Option<Table>,
}

impl Renderer {
    fn list_indent(&self) -> usize {
        self.lists.last().map(|l| l.content_indent).unwrap_or(0)
    }

    fn quote_prefix(&self) -> Option<Span<'static>> {
        (self.quote_depth > 0).then(|| {
            Span::styled("│ ".repeat(self.quote_depth), Style::default().fg(BORDER))
        })
    }

    fn flush(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        let mut spans = std::mem::take(&mut self.spans);
        let mut indent = self.list_indent();
        if let Some(prefix) = self.quote_prefix() {
            spans.insert(0, prefix);
            indent += 2 * self.quote_depth;
        }
        self.out.extend(wrap_line(spans, indent, self.width));
    }

    /// Гарантирует одну пустую строку-разделитель (без дублей).
    fn blank(&mut self) {
        self.flush();
        if self.out.last().is_some_and(|l| l.width() > 0) {
            self.out.push(Line::default());
        }
    }

    fn text_style(&self) -> Style {
        let mut s = Style::default().fg(TEXT);
        if self.quote_depth > 0 {
            s = s.fg(Color::Gray).add_modifier(Modifier::ITALIC);
        }
        if let Some(level) = self.heading {
            s = match level {
                HeadingLevel::H1 => s.fg(ACCENT_BLUE).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                HeadingLevel::H2 => s.fg(ACCENT_CYAN).add_modifier(Modifier::BOLD),
                HeadingLevel::H3 => s.fg(Color::White).add_modifier(Modifier::BOLD),
                _ => s.fg(Color::Gray).add_modifier(Modifier::BOLD),
            };
        }
        if self.bold {
            s = s.fg(Color::White).add_modifier(Modifier::BOLD);
        }
        if self.italic {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.strike {
            s = s.add_modifier(Modifier::CROSSED_OUT);
        }
        if self.link {
            s = s.fg(ACCENT_CYAN).add_modifier(Modifier::UNDERLINED);
        }
        s
    }

    fn push(&mut self, span: Span<'static>) {
        // Новый абзац внутри элемента списка выравнивается по тексту элемента.
        if self.spans.is_empty() && !self.lists.is_empty() {
            self.spans.push(Span::raw(" ".repeat(self.list_indent())));
        }
        self.spans.push(span);
    }

    fn render_code_block(&mut self, lang: &str, code: &str) {
        let base = self.list_indent();
        let pad = " ".repeat(base);
        let border = Style::default().fg(BORDER);
        let label = if lang.is_empty() { "code" } else { lang };
        self.out.push(Line::from(vec![
            Span::raw(pad.clone()),
            Span::styled("╭─ ", border),
            Span::styled(label.to_string(), Style::default().fg(Color::Gray)),
        ]));
        let inner_w = self.width.saturating_sub(base + 2).max(1);
        let code_style = Style::default().fg(CODE_TEXT);
        for raw in code.trim_end_matches('\n').split('\n') {
            let text = raw.replace('\t', "    ");
            for mut line in wrap_line(vec![Span::styled(text, code_style)], 0, inner_w) {
                line.spans.insert(0, Span::styled("│ ", border));
                line.spans.insert(0, Span::raw(pad.clone()));
                self.out.push(line);
            }
        }
        self.out.push(Line::from(vec![Span::raw(pad), Span::styled("╰─", border)]));
    }

    fn render_table(&mut self, table: Table) {
        let rows: Vec<Vec<String>> = table
            .rows
            .into_iter()
            .map(|r| r.into_iter().map(|c| c.split_whitespace().collect::<Vec<_>>().join(" ")).collect())
            .collect();
        let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if cols == 0 {
            return;
        }
        let mut widths = vec![1usize; cols];
        for row in &rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell.width());
            }
        }
        let base = self.list_indent();
        let pad = " ".repeat(base);
        let total: usize = widths.iter().sum::<usize>() + 3 * cols + 1;
        let border = Style::default().fg(BORDER);
        let head_style = Style::default().fg(ACCENT_CYAN).add_modifier(Modifier::BOLD);
        let cell_style = Style::default().fg(TEXT);

        if base + total <= self.width {
            let rule = |l: &str, m: &str, r: &str| -> Line<'static> {
                let mid: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
                Line::from(vec![
                    Span::raw(pad.clone()),
                    Span::styled(format!("{l}{}{r}", mid.join(m)), border),
                ])
            };
            self.out.push(rule("┌", "┬", "┐"));
            for (ri, row) in rows.iter().enumerate() {
                let is_head = table.has_header && ri == 0;
                let mut spans = vec![Span::raw(pad.clone()), Span::styled("│", border)];
                for (ci, w) in widths.iter().enumerate() {
                    let cell = row.get(ci).map(String::as_str).unwrap_or("");
                    let fill = " ".repeat(w - cell.width());
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(cell.to_string(), if is_head { head_style } else { cell_style }));
                    spans.push(Span::raw(format!("{fill} ")));
                    spans.push(Span::styled("│", border));
                }
                self.out.push(Line::from(spans));
                if is_head {
                    self.out.push(rule("├", "┼", "┤"));
                }
            }
            self.out.push(rule("└", "┴", "┘"));
        } else {
            // Таблица не влезает по ширине: выводим каждую строку карточкой «Заголовок: значение».
            let (header, body) = if table.has_header {
                (rows[0].clone(), &rows[1..])
            } else {
                (Vec::new(), &rows[..])
            };
            for (ri, row) in body.iter().enumerate() {
                if ri > 0 {
                    self.out.push(Line::from(vec![
                        Span::raw(pad.clone()),
                        Span::styled("─".repeat(self.width.saturating_sub(base).min(20)), border),
                    ]));
                }
                for (ci, cell) in row.iter().enumerate() {
                    let mut spans = vec![Span::raw(pad.clone())];
                    match header.get(ci).filter(|h| !h.is_empty()) {
                        Some(h) => spans.push(Span::styled(format!("{h}: "), head_style)),
                        None => spans.push(Span::styled("• ", head_style)),
                    }
                    spans.push(Span::styled(cell.clone(), cell_style));
                    self.out.extend(wrap_line(spans, base + 2, self.width));
                }
            }
        }
    }
}

/// Превращает Markdown в строки терминала, уже перенесённые под ширину `width`.
pub fn render_markdown(markdown: &str, width: usize) -> Vec<Line<'static>> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let mut r = Renderer {
        width: width.max(10),
        out: Vec::new(),
        spans: Vec::new(),
        bold: false,
        italic: false,
        strike: false,
        link: false,
        heading: None,
        quote_depth: 0,
        lists: Vec::new(),
        code: None,
        table: None,
    };

    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    r.blank();
                    r.heading = Some(level);
                }
                Tag::Strong => r.bold = true,
                Tag::Emphasis => r.italic = true,
                Tag::Strikethrough => r.strike = true,
                Tag::Link { .. } => r.link = true,
                Tag::BlockQuote(_) => {
                    r.flush();
                    r.quote_depth += 1;
                }
                Tag::CodeBlock(kind) => {
                    r.flush();
                    let lang = match kind {
                        CodeBlockKind::Fenced(l) => l.split_whitespace().next().unwrap_or("").to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    r.code = Some((lang, String::new()));
                }
                Tag::List(start) => {
                    r.flush();
                    let base = r.list_indent();
                    r.lists.push(ListState {
                        kind: match start {
                            Some(n) => ListKind::Ordered(n),
                            None => ListKind::Unordered,
                        },
                        base,
                        content_indent: base + 2,
                    });
                }
                Tag::Item => {
                    r.flush();
                    if let Some(list) = r.lists.last_mut() {
                        let marker = match &mut list.kind {
                            ListKind::Ordered(n) => {
                                let m = format!("{n}. ");
                                *n += 1;
                                m
                            }
                            ListKind::Unordered => "• ".to_string(),
                        };
                        list.content_indent = list.base + marker.width();
                        let base = list.base;
                        r.spans.push(Span::raw(" ".repeat(base)));
                        r.spans.push(Span::styled(
                            marker,
                            Style::default().fg(ACCENT_BLUE).add_modifier(Modifier::BOLD),
                        ));
                    }
                }
                Tag::Table(_) => {
                    r.blank();
                    r.table = Some(Table::default());
                }
                Tag::TableHead => {
                    if let Some(t) = r.table.as_mut() {
                        t.has_header = true;
                        t.rows.push(Vec::new());
                    }
                }
                Tag::TableRow => {
                    if let Some(t) = r.table.as_mut() {
                        t.rows.push(Vec::new());
                    }
                }
                Tag::TableCell => {
                    if let Some(t) = r.table.as_mut() {
                        t.cell.clear();
                    }
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Paragraph => {
                    r.flush();
                    if r.lists.is_empty() {
                        r.blank();
                    }
                }
                TagEnd::Heading(_) => {
                    r.flush();
                    r.heading = None;
                    r.blank();
                }
                TagEnd::Strong => r.bold = false,
                TagEnd::Emphasis => r.italic = false,
                TagEnd::Strikethrough => r.strike = false,
                TagEnd::Link => r.link = false,
                TagEnd::BlockQuote(_) => {
                    r.flush();
                    r.quote_depth = r.quote_depth.saturating_sub(1);
                    if r.quote_depth == 0 {
                        r.blank();
                    }
                }
                TagEnd::CodeBlock => {
                    if let Some((lang, code)) = r.code.take() {
                        r.render_code_block(&lang, &code);
                    }
                    if r.lists.is_empty() {
                        r.blank();
                    }
                }
                TagEnd::Item => r.flush(),
                TagEnd::List(_) => {
                    r.flush();
                    r.lists.pop();
                    if r.lists.is_empty() {
                        r.blank();
                    }
                }
                TagEnd::TableCell => {
                    if let Some(t) = r.table.as_mut() {
                        let cell = std::mem::take(&mut t.cell);
                        if let Some(row) = t.rows.last_mut() {
                            row.push(cell);
                        }
                    }
                }
                TagEnd::Table => {
                    if let Some(t) = r.table.take() {
                        r.render_table(t);
                    }
                    r.blank();
                }
                _ => {}
            },
            Event::Text(text) => {
                if let Some((_, code)) = r.code.as_mut() {
                    code.push_str(&text);
                } else if let Some(t) = r.table.as_mut() {
                    t.cell.push_str(&text);
                } else {
                    let style = r.text_style();
                    r.push(Span::styled(text.to_string(), style));
                }
            }
            Event::Code(code) => {
                if let Some(t) = r.table.as_mut() {
                    t.cell.push_str(&code);
                } else {
                    r.push(Span::styled(
                        format!(" {code} "),
                        Style::default().fg(INLINE_CODE).bg(CODE_BG),
                    ));
                }
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                if r.table.is_none() && r.code.is_none() {
                    let style = r.text_style();
                    r.push(Span::styled(html.to_string(), style));
                }
            }
            Event::TaskListMarker(done) => {
                let (mark, color) = if done { ("[x] ", Color::LightGreen) } else { ("[ ] ", Color::Gray) };
                r.push(Span::styled(mark, Style::default().fg(color)));
            }
            Event::SoftBreak => {
                if let Some(t) = r.table.as_mut() {
                    t.cell.push(' ');
                } else {
                    r.push(Span::raw(" "));
                }
            }
            Event::HardBreak => {
                if r.table.is_none() {
                    r.flush();
                }
            }
            Event::Rule => {
                r.blank();
                let w = r.width.min(60);
                r.out.push(Line::from(Span::styled("─".repeat(w), Style::default().fg(BORDER))));
                r.blank();
            }
            _ => {}
        }
    }

    r.flush();
    while r.out.last().is_some_and(|l| l.width() == 0) {
        r.out.pop();
    }
    r.out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn wrap_respects_width_and_indent() {
        let text = "Очень длинная строка, которая обязательно должна перенестись на несколько строк";
        let lines = wrap_line(vec![Span::raw("• "), Span::raw(text)], 2, 20);
        let out = plain(&lines);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|l| l.width() <= 20), "{out:?}");
        assert!(out[1..].iter().all(|l| l.starts_with("  ")), "{out:?}");
    }

    #[test]
    fn long_word_is_split() {
        let lines = wrap_line(vec![Span::raw("a".repeat(45))], 0, 20);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| l.width() <= 20));
    }

    #[test]
    fn renders_markdown_blocks() {
        let md = "# Заголовок\n\nТекст с **жирным** и `кодом`.\n\n* пункт\n    * вложенный пункт с длинным текстом для переноса\n* второй\n\n```rust\nfn main() {}\n```\n\n| A | Bb |\n|---|---|\n| 1 | 2 |\n\n> цитата\n";
        let lines = render_markdown(md, 30);
        let out = plain(&lines);
        for l in &out {
            println!("{l}");
        }
        assert!(lines.iter().all(|l| l.width() <= 30), "{out:?}");
        assert!(out.iter().any(|l| l.starts_with("  • вложенный")), "{out:?}");
        assert!(out.iter().any(|l| l.starts_with("╭─ rust")), "{out:?}");
        assert!(out.iter().any(|l| l.contains("│ 1 │ 2  │")), "{out:?}");
        assert!(out.iter().any(|l| l.starts_with("│ цитата")), "{out:?}");
    }
}
