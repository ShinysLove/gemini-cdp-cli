use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

const ACCENT_BLUE: Color = Color::Rgb(20, 120, 255);
const CODE_BG: Color = Color::Rgb(20, 25, 35);

enum ListKind {
    Ordered(usize),
    Unordered,
}

pub fn parse_markdown_to_lines(markdown: &str) -> Vec<Line<'static>> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(markdown, options);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();

    let mut is_bold = false;
    let mut is_italic = false;
    let mut in_code_block = false;
    let mut list_stack: Vec<ListKind> = Vec::new();

    let mut in_table = false;
    let mut table_headers: Vec<String> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell = String::new();
    let mut in_header = false;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { .. } => {
                    if !current_spans.is_empty() {
                        lines.push(Line::from(std::mem::take(&mut current_spans)));
                    }
                    is_bold = true;
                }
                Tag::Strong => is_bold = true,
                Tag::Emphasis => is_italic = true,
                Tag::CodeBlock(_) => {
                    if !current_spans.is_empty() {
                        lines.push(Line::from(std::mem::take(&mut current_spans)));
                    }
                    in_code_block = true;
                }
                Tag::List(start_num) => match start_num {
                    Some(n) => list_stack.push(ListKind::Ordered(n as usize)),
                    None => list_stack.push(ListKind::Unordered),
                },
                Tag::Item => {
                    let depth = list_stack.len().saturating_sub(1);
                    let indent = "  ".repeat(depth);
                    let prefix = match list_stack.last_mut() {
                        Some(ListKind::Ordered(idx)) => {
                            let p = format!("{}. ", idx);
                            *idx += 1;
                            p
                        }
                        Some(ListKind::Unordered) | None => "• ".to_string(),
                    };
                    current_spans.push(Span::raw(indent));
                    current_spans.push(Span::styled(
                        prefix,
                        Style::default().fg(ACCENT_BLUE).add_modifier(Modifier::BOLD),
                    ));
                }
                Tag::Table(_) => {
                    if !current_spans.is_empty() {
                        lines.push(Line::from(std::mem::take(&mut current_spans)));
                    }
                    in_table = true;
                    table_headers.clear();
                }
                Tag::TableHead => {
                    in_header = true;
                    current_row.clear();
                }
                Tag::TableRow => {
                    in_header = false;
                    current_row.clear();
                }
                Tag::TableCell => {
                    current_cell.clear();
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Paragraph => {
                    if !current_spans.is_empty() {
                        lines.push(Line::from(std::mem::take(&mut current_spans)));
                    }
                    lines.push(Line::from(""));
                }
                TagEnd::Heading(_) => {
                    is_bold = false;
                    if !current_spans.is_empty() {
                        lines.push(Line::from(std::mem::take(&mut current_spans)));
                    }
                    lines.push(Line::from(""));
                }
                TagEnd::Item => {
                    if !current_spans.is_empty() {
                        lines.push(Line::from(std::mem::take(&mut current_spans)));
                    }
                }
                TagEnd::Strong => is_bold = false,
                TagEnd::Emphasis => is_italic = false,
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    lines.push(Line::from(""));
                }
                TagEnd::List(_) => {
                    list_stack.pop();
                    if list_stack.is_empty() {
                        lines.push(Line::from(""));
                    }
                }
                TagEnd::TableCell => {
                    if in_header {
                        table_headers.push(std::mem::take(&mut current_cell));
                    } else {
                        current_row.push(std::mem::take(&mut current_cell));
                    }
                }
                TagEnd::TableHead => {
                    let mut row_spans = vec![Span::styled("| ", Style::default().fg(Color::DarkGray))];
                    for (i, h) in table_headers.iter().enumerate() {
                        row_spans.push(Span::styled(
                            h.clone(),
                            Style::default().fg(ACCENT_BLUE).add_modifier(Modifier::BOLD),
                        ));
                        if i + 1 < table_headers.len() {
                            row_spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
                        }
                    }
                    row_spans.push(Span::styled(" |", Style::default().fg(Color::DarkGray)));
                    lines.push(Line::from(row_spans));

                    let mut sep_spans = vec![Span::styled("|", Style::default().fg(Color::DarkGray))];
                    for h in &table_headers {
                        let dashes = "-".repeat(h.len().max(3) + 2);
                        sep_spans.push(Span::styled(dashes, Style::default().fg(Color::DarkGray)));
                        sep_spans.push(Span::styled("|", Style::default().fg(Color::DarkGray)));
                    }
                    lines.push(Line::from(sep_spans));
                }
                TagEnd::TableRow => {
                    let mut row_spans = vec![Span::styled("| ", Style::default().fg(Color::DarkGray))];
                    for (i, c) in current_row.iter().enumerate() {
                        row_spans.push(Span::styled(c.clone(), Style::default().fg(Color::White)));
                        if i + 1 < current_row.len() {
                            row_spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
                        }
                    }
                    row_spans.push(Span::styled(" |", Style::default().fg(Color::DarkGray)));
                    lines.push(Line::from(row_spans));
                }
                TagEnd::Table => {
                    in_table = false;
                    lines.push(Line::from(""));
                }
                _ => {}
            },
            Event::Text(text) => {
                if in_table {
                    current_cell.push_str(&text);
                    continue;
                }

                if in_code_block {
                    for line_str in text.lines() {
                        lines.push(Line::from(vec![
                            Span::raw("    "),
                            Span::styled(
                                line_str.to_string(),
                                Style::default().fg(Color::LightGreen).bg(CODE_BG),
                            ),
                        ]));
                    }
                    continue;
                }

                let mut style = Style::default().fg(Color::White);
                if is_bold {
                    style = style.add_modifier(Modifier::BOLD).fg(Color::Cyan);
                }
                if is_italic {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                current_spans.push(Span::styled(text.to_string(), style));
            }
            Event::Code(code_text) => {
                if in_table {
                    current_cell.push('`');
                    current_cell.push_str(&code_text);
                    current_cell.push('`');
                    continue;
                }
                let style = Style::default().fg(Color::Yellow).bg(CODE_BG);
                current_spans.push(Span::styled(format!(" `{code_text}` "), style));
            }
            Event::SoftBreak => {
                if in_table {
                    current_cell.push(' ');
                } else {
                    current_spans.push(Span::raw(" "));
                }
            }
            Event::HardBreak => {
                if !in_table {
                    lines.push(Line::from(std::mem::take(&mut current_spans)));
                }
            }
            _ => {}
        }
    }

    if !current_spans.is_empty() {
        lines.push(Line::from(current_spans));
    }

    lines
}