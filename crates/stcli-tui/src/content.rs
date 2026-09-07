//! Candidate Markdown → HTML → styled terminal text (ADR 0013).

use std::{borrow::Cow, collections::VecDeque};

use cssparser::{DeclarationParser, Parser as CssParser, ParserInput, RuleBodyParser};
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd, html};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};
use scraper::{ElementRef, Html, Node};

use crate::theme::Theme;

/// Larger sources are displayed literally, without parsing.
pub const MAX_INPUT_BYTES: usize = 256 * 1024;
/// Maximum nested element depth, excluding the fragment root.
pub const MAX_TREE_DEPTH: usize = 128;

/// Reserved color policies. Only Preserve is implemented; render uses its default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColorMode {
    #[default]
    Preserve,
    Semantic,
    Contrast,
}

/// Render one Candidate. Link references are local to this call.
/// Oversize or over-deep sources fall back to literal, control-neutralized text.
pub fn render(source: &str, theme: Theme) -> Text<'static> {
    if source.len() > MAX_INPUT_BYTES {
        return plain_source(source, theme);
    }
    let safe_source = TerminalFilter::default().clean(source);
    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM;
    let mut html_source = String::with_capacity(source.len());
    html::push_html(
        &mut html_source,
        Autolinks::new(Parser::new_ext(&safe_source, options)),
    );
    let document = Html::parse_fragment(&html_source);
    let mut renderer = Renderer {
        lines: vec![Line::default()],
        links: Vec::new(),
        lists: Vec::new(),
        controls: TerminalFilter::default(),
        theme,
    };
    if renderer
        .element(
            document.root_element(),
            Style::default().fg(theme.foreground),
            false,
            0,
        )
        .is_err()
    {
        return plain_source(source, theme);
    }
    renderer.line_break();
    renderer.lines.pop();
    if !renderer.links.is_empty() {
        renderer.lines.push(Line::default());
        for (index, address) in renderer.links.iter().enumerate() {
            renderer.lines.push(Line::styled(
                format!("[{}] {address}", index + 1),
                Style::default().fg(theme.muted),
            ));
        }
    }
    Text::from(renderer.lines)
}

fn plain_source(source: &str, theme: Theme) -> Text<'static> {
    Text::from(
        TerminalFilter::default()
            .clean(source)
            .split('\n')
            .map(|line| Line::styled(line.to_owned(), Style::default().fg(theme.foreground)))
            .collect::<Vec<_>>(),
    )
}

// Keep state across DOM text nodes: an entity-encoded OSC/CSI can straddle tags.
#[derive(Default, PartialEq)]
enum EscapeState {
    #[default]
    Text,
    Escape,
    Intermediate,
    Csi,
    String,
    StringEscape,
}

#[derive(Default)]
struct TerminalFilter {
    state: EscapeState,
}

impl TerminalFilter {
    fn clean<'a>(&mut self, source: &'a str) -> Cow<'a, str> {
        if self.state == EscapeState::Text && !source.chars().any(char::is_control) {
            return Cow::Borrowed(source);
        }
        let mut result = String::with_capacity(source.len());
        for ch in source.chars() {
            match self.state {
                EscapeState::String | EscapeState::StringEscape => {
                    self.state = match ch {
                        '\x07' | '\u{9c}' => EscapeState::Text,
                        '\\' if self.state == EscapeState::StringEscape => EscapeState::Text,
                        '\x1b' => EscapeState::StringEscape,
                        _ => EscapeState::String,
                    };
                }
                _ if ch == '\x1b' => self.state = EscapeState::Escape,
                _ if ch == '\u{9b}' => self.state = EscapeState::Csi,
                _ if matches!(ch, '\u{90}' | '\u{98}' | '\u{9d}' | '\u{9e}' | '\u{9f}') => {
                    self.state = EscapeState::String
                }
                EscapeState::Escape => {
                    self.state = match ch {
                        '[' => EscapeState::Csi,
                        ']' | 'P' | 'X' | '^' | '_' => EscapeState::String,
                        ' '..='/' => EscapeState::Intermediate,
                        _ => EscapeState::Text,
                    }
                }
                EscapeState::Intermediate => {
                    if matches!(ch, '0'..='~') {
                        self.state = EscapeState::Text;
                    }
                }
                EscapeState::Csi => {
                    if matches!(ch, '@'..='~') {
                        self.state = EscapeState::Text;
                    }
                }
                EscapeState::Text => match ch {
                    '\t' => result.push(' '),
                    '\n' => result.push('\n'),
                    _ if ch.is_control() => {}
                    _ => result.push(ch),
                },
            }
        }
        Cow::Owned(result)
    }
}

// pulldown-cmark's GFM option does not implement bare URL/email autolinks.
// Adapt text events only; code, image descriptions and existing links stay intact.
struct Autolinks<'a> {
    parser: Parser<'a>,
    finder: linkify::LinkFinder,
    pending: VecDeque<Event<'a>>,
    protected: usize,
    raw_protected: u8,
}

impl<'a> Autolinks<'a> {
    fn new(parser: Parser<'a>) -> Self {
        let mut finder = linkify::LinkFinder::new();
        finder.url_must_have_scheme(false);
        Self {
            parser,
            finder,
            pending: VecDeque::new(),
            protected: 0,
            raw_protected: 0,
        }
    }
}

impl<'a> Iterator for Autolinks<'a> {
    type Item = Event<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(event) = self.pending.pop_front() {
            return Some(event);
        }
        let event = self.parser.next()?;
        match &event {
            Event::Start(Tag::Link { .. } | Tag::Image { .. } | Tag::CodeBlock(_)) => {
                self.protected += 1
            }
            Event::End(TagEnd::Link | TagEnd::Image | TagEnd::CodeBlock) => self.protected -= 1,
            Event::InlineHtml(raw) => {
                // pulldown has already recognized a raw HTML tag. Inspect only its
                // name to avoid generating nested anchors or links inside raw code.
                if let Some(tag) = raw.strip_prefix('<') {
                    let (closing, tag) = tag
                        .strip_prefix('/')
                        .map_or((false, tag), |tag| (true, tag));
                    let name = tag
                        .split([' ', '\t', '\n', '\r', '/', '>'])
                        .next()
                        .unwrap_or_default();
                    let bit = if name.eq_ignore_ascii_case("a") {
                        1
                    } else if name.eq_ignore_ascii_case("code") {
                        2
                    } else if name.eq_ignore_ascii_case("pre") {
                        4
                    } else if name.eq_ignore_ascii_case("kbd") {
                        8
                    } else {
                        0
                    };
                    if closing {
                        self.raw_protected &= !bit;
                    } else {
                        self.raw_protected |= bit;
                    }
                }
            }
            Event::Text(text) if self.protected == 0 && self.raw_protected == 0 => {
                let mut end = 0;
                for link in self.finder.links(text) {
                    // GFM recognizes www., http(s), and email, not arbitrary bare domains.
                    let www = link
                        .as_str()
                        .get(..4)
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("www."));
                    let http = link
                        .as_str()
                        .get(..7)
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
                        || link
                            .as_str()
                            .get(..8)
                            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"));
                    if *link.kind() == linkify::LinkKind::Url && !www && !http {
                        continue;
                    }
                    if link.start() > end {
                        self.pending
                            .push_back(Event::Text(text[end..link.start()].to_owned().into()));
                    }
                    let address = if *link.kind() == linkify::LinkKind::Email {
                        format!("mailto:{}", link.as_str())
                    } else if www {
                        format!("http://{}", link.as_str())
                    } else {
                        link.as_str().to_owned()
                    };
                    self.pending.push_back(Event::Start(Tag::Link {
                        link_type: LinkType::Autolink,
                        dest_url: address.into(),
                        title: "".into(),
                        id: "".into(),
                    }));
                    self.pending
                        .push_back(Event::Text(link.as_str().to_owned().into()));
                    self.pending.push_back(Event::End(TagEnd::Link));
                    end = link.end();
                }
                if end != 0 {
                    if end < text.len() {
                        self.pending
                            .push_back(Event::Text(text[end..].to_owned().into()));
                    }
                    return self.pending.pop_front();
                }
            }
            _ => {}
        }
        Some(event)
    }
}

struct Renderer {
    lines: Vec<Line<'static>>,
    links: Vec<String>,
    lists: Vec<Option<u64>>,
    controls: TerminalFilter,
    theme: Theme,
}

impl Renderer {
    fn element(
        &mut self,
        element: ElementRef<'_>,
        mut style: Style,
        pre: bool,
        depth: usize,
    ) -> Result<(), ()> {
        if depth > MAX_TREE_DEPTH {
            return Err(());
        }
        let name = element.value().name();
        let css = InlineStyle::parse(element.attr("style").unwrap_or_default());
        if matches!(name, "script" | "style" | "template" | "head")
            || element.attr("hidden").is_some()
            || element
                .attr("aria-hidden")
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
            || css.display_none
            || css.visibility_hidden
            || css
                .foreground
                .is_some_and(|color| Some(color) == css.background)
        {
            return Ok(());
        }
        let block = matches!(
            name,
            "p" | "div"
                | "section"
                | "article"
                | "header"
                | "footer"
                | "main"
                | "aside"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "summary"
                | "details"
                | "blockquote"
                | "pre"
                | "ul"
                | "ol"
                | "li"
                | "table"
                | "tr"
                | "hr"
        );
        let first_item_paragraph = name == "p"
            && element
                .prev_siblings()
                .all(|node| !node.value().is_element())
            && element
                .parent()
                .and_then(ElementRef::wrap)
                .is_some_and(|parent| parent.value().name() == "li");
        if block && !first_item_paragraph {
            self.line_break();
        }
        match name {
            "strong" | "b" | "th" => style = style.add_modifier(Modifier::BOLD),
            "em" | "i" => style = style.add_modifier(Modifier::ITALIC),
            "del" | "s" | "strike" => style = style.add_modifier(Modifier::CROSSED_OUT),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "summary" => {
                style = style.fg(self.theme.accent).add_modifier(Modifier::BOLD)
            }
            "code" => style = style.fg(self.theme.accent).bg(self.theme.selection),
            "blockquote" => self.push("│ ".to_owned(), Style::default().fg(self.theme.muted)),
            "ul" => self.lists.push(None),
            "ol" => self.lists.push(Some(
                element
                    .attr("start")
                    .and_then(|start| start.parse().ok())
                    .unwrap_or(1),
            )),
            "li" => {
                let indent = "  ".repeat(self.lists.len().saturating_sub(1));
                let marker = match self.lists.last_mut() {
                    Some(Some(next)) => {
                        let number = element
                            .attr("value")
                            .and_then(|value| value.parse().ok())
                            .unwrap_or(*next);
                        *next = number.saturating_add(1);
                        format!("{number}. ")
                    }
                    _ => "• ".to_owned(),
                };
                self.push(
                    format!("{indent}{marker}"),
                    Style::default().fg(self.theme.muted),
                );
            }
            "br" => {
                self.lines.push(Line::default());
            }
            "hr" => self.push(
                "────────────────".to_owned(),
                Style::default().fg(self.theme.muted),
            ),
            "img" => {
                let alt = element.attr("alt").unwrap_or_default();
                self.text(
                    &if alt.is_empty() {
                        "[image]".to_owned()
                    } else {
                        format!("[image: {alt}]")
                    },
                    style,
                    false,
                );
            }
            "input" if element.attr("type") == Some("checkbox") => self.push(
                if element.attr("checked").is_some() {
                    "[x] "
                } else {
                    "[ ] "
                }
                .to_owned(),
                style,
            ),
            _ => {}
        }
        if matches!(name, "td" | "th") && !self.lines.last().unwrap().spans.is_empty() {
            self.trim_line();
            self.push(" │ ".to_owned(), style);
        }
        if let Some(color) = css.foreground.or_else(|| {
            (name == "font")
                .then(|| element.attr("color").and_then(parse_color))
                .flatten()
        }) {
            style = style.fg(color);
        }
        for child in element.children() {
            match child.value() {
                Node::Text(text) => self.text(text, style, pre || name == "pre"),
                Node::Element(_) => self.element(
                    ElementRef::wrap(child).unwrap(),
                    style,
                    pre || name == "pre",
                    depth + 1,
                )?,
                _ => {}
            }
        }
        if name == "a"
            && let Some(address) = element.attr("href")
        {
            self.links
                .push(TerminalFilter::default().clean(address).replace('\n', " "));
            self.push(format!(" [{}]", self.links.len()), style);
        }
        if matches!(name, "ul" | "ol") {
            self.lists.pop();
        }
        if block {
            self.line_break();
        }
        Ok(())
    }

    fn text(&mut self, source: &str, style: Style, pre: bool) {
        if pre {
            for (index, line) in source.split('\n').enumerate() {
                if index != 0 {
                    self.lines.push(Line::default());
                }
                self.push(line.to_owned(), style);
            }
            return;
        }
        let mut text = String::with_capacity(source.len());
        let mut space = self
            .lines
            .last()
            .unwrap()
            .spans
            .last()
            .is_none_or(|span| span.content.ends_with(' '));
        for ch in source.chars() {
            if matches!(ch, ' ' | '\n' | '\r' | '\t' | '\x0c') {
                if !space {
                    text.push(' ');
                }
                space = true;
            } else {
                text.push(ch);
                space = false;
            }
        }
        self.push(text, style);
    }

    fn push(&mut self, text: String, style: Style) {
        let text = match self.controls.clean(&text) {
            Cow::Borrowed(_) => text,
            Cow::Owned(safe) => safe,
        };
        if !text.is_empty() {
            self.lines
                .last_mut()
                .unwrap()
                .spans
                .push(Span::styled(text, style));
        }
    }

    fn trim_line(&mut self) {
        let spans = &mut self.lines.last_mut().unwrap().spans;
        while let Some(span) = spans.last_mut() {
            let length = span.content.trim_end_matches(' ').len();
            if length != 0 {
                if length < span.content.len() {
                    span.content.to_mut().truncate(length);
                }
                break;
            }
            spans.pop();
        }
    }

    fn line_break(&mut self) {
        self.trim_line();
        if !self.lines.last().unwrap().spans.is_empty() {
            self.lines.push(Line::default());
        }
    }
}

#[derive(Default)]
struct InlineStyle {
    foreground: Option<Color>,
    background: Option<Color>,
    display_none: bool,
    visibility_hidden: bool,
}

impl InlineStyle {
    fn parse(source: &str) -> Self {
        let mut input = ParserInput::new(source);
        let mut parser = CssParser::new(&mut input);
        let mut declarations = Declarations;
        let mut result = Self::default();
        let mut important = [false; 4];
        for (property, priority) in RuleBodyParser::new(&mut parser, &mut declarations).flatten() {
            let index = match property {
                Property::Foreground(_) => 0,
                Property::Background(_) => 1,
                Property::Display(_) => 2,
                Property::Visibility(_) => 3,
            };
            if priority || !important[index] {
                match property {
                    Property::Foreground(color) => result.foreground = color,
                    Property::Background(color) => result.background = color,
                    Property::Display(hidden) => result.display_none = hidden,
                    Property::Visibility(hidden) => result.visibility_hidden = hidden,
                }
                important[index] = priority;
            }
        }
        result
    }
}

enum Property {
    Foreground(Option<Color>),
    Background(Option<Color>),
    Display(bool),
    Visibility(bool),
}

struct Declarations;
impl<'i> DeclarationParser<'i> for Declarations {
    type Declaration = (Property, bool);
    type Error = ();
    fn parse_value<'t>(
        &mut self,
        name: cssparser::CowRcStr<'i>,
        input: &mut CssParser<'i, 't>,
        _: &cssparser::ParserState,
    ) -> Result<Self::Declaration, cssparser::ParseError<'i, ()>> {
        let property = if name.eq_ignore_ascii_case("color") {
            Property::Foreground(rgb(cssparser_color::Color::parse(input)?))
        } else if name.eq_ignore_ascii_case("background-color") {
            Property::Background(rgb(cssparser_color::Color::parse(input)?))
        } else if name.eq_ignore_ascii_case("background") {
            // The color is the final layer's color; other shorthand tokens do not
            // affect this same-element RGB comparison. Never load image URLs.
            let mut color = None;
            input.parse_until_before(cssparser::Delimiter::Bang, |input| {
                while !input.is_exhausted() {
                    if let Ok(parsed) = input.try_parse(cssparser_color::Color::parse) {
                        color = rgb(parsed);
                    } else {
                        input.next()?;
                    }
                }
                Ok::<_, cssparser::ParseError<'i, ()>>(())
            })?;
            Property::Background(color)
        } else if name.eq_ignore_ascii_case("display") {
            let value = input.expect_ident()?;
            if ![
                "none",
                "block",
                "inline",
                "inline-block",
                "flow-root",
                "flex",
                "inline-flex",
                "grid",
                "inline-grid",
                "table",
                "inline-table",
                "table-row",
                "table-cell",
                "table-caption",
                "table-row-group",
                "table-header-group",
                "table-footer-group",
                "table-column",
                "table-column-group",
                "list-item",
                "contents",
                "inherit",
                "initial",
                "unset",
                "revert",
                "revert-layer",
            ]
            .iter()
            .any(|allowed| value.eq_ignore_ascii_case(allowed))
            {
                return Err(input.new_custom_error(()));
            }
            Property::Display(value.eq_ignore_ascii_case("none"))
        } else if name.eq_ignore_ascii_case("visibility") {
            let value = input.expect_ident()?;
            if ![
                "visible",
                "hidden",
                "collapse",
                "inherit",
                "initial",
                "unset",
                "revert",
                "revert-layer",
            ]
            .iter()
            .any(|allowed| value.eq_ignore_ascii_case(allowed))
            {
                return Err(input.new_custom_error(()));
            }
            Property::Visibility(value.eq_ignore_ascii_case("hidden"))
        } else {
            return Err(input.new_custom_error(()));
        };
        let important = input.try_parse(cssparser::parse_important).is_ok();
        input.expect_exhausted()?;
        Ok((property, important))
    }
}
impl<'i> cssparser::AtRuleParser<'i> for Declarations {
    type Prelude = ();
    type AtRule = (Property, bool);
    type Error = ();
}
impl<'i> cssparser::QualifiedRuleParser<'i> for Declarations {
    type Prelude = ();
    type QualifiedRule = (Property, bool);
    type Error = ();
}
impl<'i> cssparser::RuleBodyItemParser<'i, (Property, bool), ()> for Declarations {
    fn parse_declarations(&self) -> bool {
        true
    }
    fn parse_qualified(&self) -> bool {
        false
    }
}

fn parse_color(source: &str) -> Option<Color> {
    let mut input = ParserInput::new(source);
    let mut parser = CssParser::new(&mut input);
    let color = cssparser_color::Color::parse(&mut parser).ok()?;
    parser.expect_exhausted().ok()?;
    rgb(color)
}

fn rgb(color: cssparser_color::Color) -> Option<Color> {
    use cssparser_color::Color as CssColor;
    let (red, green, blue) = match color {
        CssColor::Rgba(value) => return Some(Color::Rgb(value.red, value.green, value.blue)),
        CssColor::Hsl(value) => cssparser_color::hsl_to_rgb(
            value.hue.unwrap_or(0.) / 360.,
            value.saturation.unwrap_or(0.),
            value.lightness.unwrap_or(0.),
        ),
        CssColor::Hwb(value) => cssparser_color::hwb_to_rgb(
            value.hue.unwrap_or(0.) / 360.,
            value.whiteness.unwrap_or(0.),
            value.blackness.unwrap_or(0.),
        ),
        _ => return None,
    };
    Some(Color::Rgb(
        cssparser::color::clamp_unit_f32(red),
        cssparser::color::clamp_unit_f32(green),
        cssparser::color::clamp_unit_f32(blue),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;

    // Ticket 15: Candidate Markdown and embedded RP-style HTML share one styled text path.
    #[test]
    fn renders_mixed_candidate_content_as_styled_text() {
        let source = concat!(
            "# Harbor Watch\n\n*calm* **steady** `signal` ~~storm~~\n\n",
            "- west pier\n\n  Second watch.\n\n",
            "| Ship | State |\n| --- | --- |\n| Dawn | Safe |\n\n",
            "<details><summary>Night report</summary><div><font color=\"#f80\">Amber</font> ",
            "<b style=\"color:rgb(12, 34, 56)\">Ready</b><br>Body always shown</div></details>\n\n",
            "[Log](https://example.org/log) and <https://example.org/watch> ![beacon](lamp.png) ![](empty.png)\n\n",
            "https://example.org/bare and <a href=\"https://example.org/target\">https://example.org/label</a>",
        );
        let text = render(source, Theme::dark());
        let plain = text.to_string();
        assert!(plain.contains("• west pier"));
        assert!(plain.contains("Ship │ State\nDawn │ Safe"));
        assert!(plain.contains("Night report\nAmber Ready\nBody always shown"));
        assert!(
            plain.contains("Log [1] and https://example.org/watch [2] [image: beacon] [image]")
        );
        assert!(plain.contains("https://example.org/bare [3] and https://example.org/label [4]"));
        assert!(plain.ends_with("[1] https://example.org/log\n[2] https://example.org/watch\n[3] https://example.org/bare\n[4] https://example.org/target"));
        let spans = text
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .collect::<Vec<_>>();
        for (word, modifier) in [
            ("calm", Modifier::ITALIC),
            ("steady", Modifier::BOLD),
            ("storm", Modifier::CROSSED_OUT),
            ("Night report", Modifier::BOLD),
        ] {
            assert!(
                spans
                    .iter()
                    .any(|span| span.content == word && span.style.add_modifier.contains(modifier)),
                "{word}"
            );
        }
        assert!(
            spans
                .iter()
                .any(|span| span.content == "signal"
                    && span.style.bg == Some(Theme::dark().selection))
        );
        assert!(spans.iter().any(|span| span.content == "Amber"
            && span.style.fg == Some(ratatui::style::Color::Rgb(255, 136, 0))));
        assert!(spans.iter().any(|span| span.content == "Ready"
            && span.style.fg == Some(ratatui::style::Color::Rgb(12, 34, 56))));
        assert_eq!(
            render("[Next](https://example.org/next)", Theme::dark()).to_string(),
            "Next [1]\n\n[1] https://example.org/next"
        );
    }

    // Ticket 15: Concealed Content and terminal commands must never escape into spans.
    #[test]
    fn suppresses_concealed_content_and_neutralizes_terminal_controls() {
        let source = concat!(
            "<div>Harbor safe<script>script-secret</script><style>style-secret</style><!--comment-secret-->",
            "<span hidden>hidden-secret</span><span aria-hidden=\"TRUE\">aria-secret</span>",
            "<span style=\"DISPLAY:/**/n\\6f ne !important;display:block\">display-secret</span>",
            "<span style=\"visibility:hidden;visibility:invalid\">visibility-secret</span>",
            "<span style=\"color:rebeccapurple;background-color:rgb(102 51 153)\">color-secret</span>",
            "<span style=\"color:#f00;background:hsl(0 100% 50%)\">shorthand-secret</span>",
            "<span style=\"color:blue;background-color:red;color:red !important;color:blue\">priority-secret</span>",
            "<span style=\"color:blue;background-color:red\">Visible</span></div>\n\n",
            "Alarm \x1b[31mred\x1b[0m \x1b]0;osc-secret\x07 \u{9d}52;c;clipboard-secret\u{9c} Done\0\x08\x7f\u{85}\tSafe\n\n",
            "<div>Encoded &#27;]0;encoded-secret&#7; &#27;<b>[32mgreen</b>&#27;[0m</div>",
        );
        let text = render(source, Theme::dark());
        let plain = text.to_string();
        assert!(plain.contains("Harbor safeVisible"), "{plain:?}");
        assert!(
            plain.contains("red") && plain.contains("Done Safe") && plain.contains("green"),
            "{plain:?}"
        );
        assert!(
            !plain.contains("secret") && !plain.contains("[31m") && !plain.contains("[32m"),
            "{plain:?}"
        );
        assert!(
            text.lines
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| !span.content.chars().any(char::is_control))
        );

        // Both bounded fallbacks retain literal source, not partially converted HTML.
        let oversized = format!(
            "<b>{}</b>\x1b]0;oversize-secret\x07\tSafe",
            "x".repeat(MAX_INPUT_BYTES)
        );
        let fallback = render(&oversized, Theme::dark()).to_string();
        assert!(fallback.starts_with("<b>") && fallback.ends_with("</b> Safe"));
        assert!(!fallback.contains("secret") && !fallback.chars().any(char::is_control));
        let deep = format!(
            "{}deep\x1b[31m\tSafe{}",
            "<div>".repeat(MAX_TREE_DEPTH + 1),
            "</div>".repeat(MAX_TREE_DEPTH + 1)
        );
        let fallback = render(&deep, Theme::dark()).to_string();
        assert!(fallback.starts_with("<div><div>") && fallback.contains("deep Safe"));
        assert!(!fallback.contains("[31m") && !fallback.chars().any(char::is_control));
    }
}
