use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span, Text},
};

use crate::theme::Theme;

pub fn render(source: &str, theme: Theme) -> Text<'static> {
    let mut lines = vec![Line::default()];
    let mut style = Style::default().fg(theme.foreground);
    let mut styles = Vec::new();
    let mut list_depth = 0usize;
    let mut code_block = false;

    for event in Parser::new_ext(source, Options::ENABLE_STRIKETHROUGH) {
        match event {
            Event::Start(tag) => {
                styles.push(style);
                match tag {
                    Tag::Strong => style = style.add_modifier(Modifier::BOLD),
                    Tag::Emphasis => style = style.add_modifier(Modifier::ITALIC),
                    Tag::Strikethrough => style = style.add_modifier(Modifier::CROSSED_OUT),
                    Tag::Heading { .. } => {
                        style = style.add_modifier(Modifier::BOLD).fg(theme.accent)
                    }
                    Tag::BlockQuote(_) => {
                        new_line_if_needed(&mut lines);
                        push(&mut lines, "│ ", Style::default().fg(theme.muted));
                    }
                    Tag::CodeBlock(_) => {
                        new_line_if_needed(&mut lines);
                        push(&mut lines, "┌ code", Style::default().fg(theme.muted));
                        lines.push(Line::default());
                        code_block = true;
                        style = Style::default().fg(theme.accent);
                    }
                    Tag::List(_) => list_depth += 1,
                    Tag::Item => {
                        new_line_if_needed(&mut lines);
                        push(
                            &mut lines,
                            format!("{}• ", "  ".repeat(list_depth.saturating_sub(1))),
                            Style::default().fg(theme.muted),
                        );
                    }
                    Tag::Paragraph if !line_is_empty(&lines) => lines.push(Line::default()),
                    _ => {}
                }
            }
            Event::End(tag) => {
                if matches!(tag, TagEnd::CodeBlock) {
                    new_line_if_needed(&mut lines);
                    push(&mut lines, "└", Style::default().fg(theme.muted));
                    code_block = false;
                }
                if matches!(tag, TagEnd::List(_)) {
                    list_depth = list_depth.saturating_sub(1);
                }
                if matches!(tag, TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item) {
                    new_line_if_needed(&mut lines);
                }
                style = styles.pop().unwrap_or_default();
            }
            Event::Text(text) => {
                for (index, part) in text.split('\n').enumerate() {
                    if index > 0 {
                        lines.push(Line::default());
                    }
                    push(&mut lines, part.to_owned(), style);
                }
            }
            Event::Code(code) => push(
                &mut lines,
                code.into_string(),
                style.bg(theme.selection).fg(theme.accent),
            ),
            Event::SoftBreak => push(&mut lines, " ", style),
            Event::HardBreak => lines.push(Line::default()),
            Event::Rule => {
                new_line_if_needed(&mut lines);
                push(
                    &mut lines,
                    "────────────────",
                    Style::default().fg(theme.muted),
                );
                lines.push(Line::default());
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                let text = strip_tags(&html);
                if !text.is_empty() {
                    push(&mut lines, text, style);
                }
            }
            Event::TaskListMarker(done) => push(
                &mut lines,
                if done { "[x] " } else { "[ ] " },
                Style::default().fg(theme.muted),
            ),
            Event::FootnoteReference(reference) => {
                push(&mut lines, format!("[{reference}]"), style)
            }
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                push(&mut lines, math.into_string(), style)
            }
        }
    }

    if code_block {
        lines.push(Line::from(Span::styled(
            "└",
            Style::default().fg(theme.muted),
        )));
    }
    while lines.last().is_some_and(|line| line.spans.is_empty()) && lines.len() > 1 {
        lines.pop();
    }
    Text::from(lines)
}

fn push(lines: &mut [Line<'static>], content: impl Into<String>, style: Style) {
    lines
        .last_mut()
        .expect("markdown renderer always has a line")
        .spans
        .push(Span::styled(content.into(), style));
}

fn new_line_if_needed(lines: &mut Vec<Line<'static>>) {
    if !line_is_empty(lines) {
        lines.push(Line::default());
    }
}

fn line_is_empty(lines: &[Line<'static>]) -> bool {
    lines.last().is_none_or(|line| line.spans.is_empty())
}

fn strip_tags(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    for character in html.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(character),
            _ => {}
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_supported_markdown_without_mutating_text() {
        let text = render("# Header\n\n**bold** and `code`\n\n- item", Theme::dark());
        let rendered = text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("Header"));
        assert!(rendered.contains("bold"));
        assert!(rendered.contains("code"));
        assert!(rendered.contains("item"));
    }

    #[test]
    fn strips_unsupported_html_but_keeps_its_text() {
        let text = render("<div>Hello <b>there</b></div>", Theme::dark());
        let rendered = text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(rendered, "Hello there");
    }
}
