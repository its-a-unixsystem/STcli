use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};

use crate::{
    app::{
        App, ChatFocus, HitAction, HitTarget, Popup, Screen, SessionListEntry, selected_candidate,
    },
    markdown,
};
use unicode_width::UnicodeWidthStr;

fn cursor_parts(value: &str, focused: bool, position: usize) -> (&str, &str) {
    if !focused {
        return (value, "");
    }
    let byte_index = value
        .char_indices()
        .nth(position)
        .map_or(value.len(), |(index, _)| index);
    value.split_at(byte_index)
}

pub fn render(frame: &mut Frame<'_>, app: &mut App) {
    frame.render_widget(
        Block::default().style(
            Style::default()
                .bg(app.theme.background)
                .fg(app.theme.foreground),
        ),
        frame.area(),
    );
    app.hit_targets.clear();
    match app.screen {
        Screen::Sessions => render_sessions(frame, app),
        Screen::Chat => render_chat(frame, app),
    }
    if app.popup.is_some() {
        render_popup(frame, app);
    }
    if let Some(toast) = &app.toast {
        let width = (toast.message.chars().count() as u16 + 4)
            .min(frame.area().width.saturating_sub(2))
            .max(20);
        let area = Rect::new(frame.area().right().saturating_sub(width + 1), 1, width, 3);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(toast.message.as_str())
                .wrap(Wrap { trim: true })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(if toast.error { " Error " } else { " Notice " }),
                )
                .style(
                    Style::default()
                        .fg(if toast.error {
                            app.theme.error
                        } else {
                            app.theme.accent
                        })
                        .bg(app.theme.background),
                ),
            area,
        );
    }
}

fn render_sessions(frame: &mut Frame<'_>, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(frame.area());
    let mut title_spans = vec![
        Span::styled(
            "STcli",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  Sessions", Style::default().fg(app.theme.foreground)),
    ];
    if app.show_branches {
        title_spans.push(Span::styled(
            "  [branches]",
            Style::default().fg(app.theme.muted),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(title_spans)).block(Block::default().borders(Borders::BOTTOM)),
        chunks[0],
    );
    let filter_title = if app.filtering {
        " Filter (typing) "
    } else {
        " Filter " /**/
    };
    frame.render_widget(
        Paragraph::new(if app.filter.is_empty() {
            "Press / to filter"
        } else {
            app.filter.as_str()
        })
        .style(Style::default().fg(if app.filter.is_empty() {
            app.theme.muted
        } else {
            app.theme.foreground
        }))
        .block(Block::default().borders(Borders::ALL).title(filter_title)),
        chunks[1],
    );
    let filtered = app.filtered_sessions();
    let entries = app.session_list_entries();
    let entry_count = entries.len();
    let entries_empty = entries.is_empty();
    let visible_rows = chunks[2].height.saturating_sub(2) as usize;
    let window_start = app
        .selected_session
        .saturating_add(1)
        .saturating_sub(visible_rows)
        .min(entry_count.saturating_sub(visible_rows));
    let header = Line::from(vec![Span::styled(
        "Session",
        Style::default()
            .fg(app.theme.accent)
            .add_modifier(Modifier::BOLD),
    )]);
    let list_items: Vec<ListItem<'_>> = entries
        .iter()
        .enumerate()
        .skip(window_start)
        .take(visible_rows)
        .map(|(index, entry)| {
            let style = if index == app.selected_session {
                Style::default()
                    .bg(app.theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            match entry {
                SessionListEntry::Session(i) => {
                    let session = &filtered[*i];
                    ListItem::new(format!(
                        "{:<20} {:16} {:16} {:>5}  {} / {}  {:>7}",
                        truncate_display(&session.display_name, 20),
                        format_date(session.created_at_ms),
                        format_date(session.modified_at_ms),
                        session.turn_count,
                        session.character_label,
                        session.persona_label,
                        session.token_count,
                    ))
                    .style(style)
                }
                SessionListEntry::Branch { branch, .. } => {
                    let parent = branch
                        .parent_branch_id
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "root".to_owned());
                    let fork = branch
                        .forked_from_turn_id
                        .map(|id| format!("fork@{}", short_id(&id.to_string())))
                        .unwrap_or_else(|| "start".to_owned());
                    let connector = if entries
                        .get(index + 1)
                        .is_none_or(|next| matches!(next, SessionListEntry::Session(_)))
                    {
                        "└─"
                    } else {
                        "├─"
                    };
                    ListItem::new(format!(
                        "  {connector} {}  parent {}  {}",
                        short_id(&branch.branch_id.to_string()),
                        short_id(&parent),
                        fork,
                    ))
                    .style(if index == app.selected_session {
                        style
                    } else {
                        Style::default().fg(app.theme.muted)
                    })
                }
            }
        })
        .collect();
    frame.render_widget(
        List::new(list_items).block(Block::default().borders(Borders::TOP).title(header)),
        chunks[2],
    );
    drop(filtered);
    for row in 0..entry_count.saturating_sub(window_start).min(visible_rows) {
        app.hit_targets.push(HitTarget {
            x: chunks[2].x,
            y: chunks[2].y + 1 + row as u16,
            width: chunks[2].width,
            height: 1,
            action: HitAction::Session(window_start + row),
        });
    }
    let empty = if app.sessions.is_empty() {
        Some("No sessions. Press n to create a new session.")
    } else if entries_empty {
        Some("No Sessions match this filter. Press Escape to clear it.")
    } else {
        None
    };
    if let Some(message) = empty {
        frame.render_widget(
            Paragraph::new(message)
                .alignment(Alignment::Center)
                .style(Style::default().fg(app.theme.muted)),
            centered(chunks[2], 70, 3),
        );
    }
    let preview = {
        let entries = app.session_list_entries();
        let filtered = app.filtered_sessions();
        entries
            .get(app.selected_session)
            .and_then(|entry| match entry {
                SessionListEntry::Session(i) => {
                    filtered.get(*i).map(|s| s.last_message_preview.clone())
                }
                SessionListEntry::Branch { session_index, .. } => filtered
                    .get(*session_index)
                    .map(|s| s.last_message_preview.clone()),
            })
            .unwrap_or_default()
    };
    if !preview.is_empty() {
        frame.render_widget(
            Paragraph::new(preview)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(app.theme.muted)),
            chunks[3],
        );
    }
    let branches_hint = if app.show_branches {
        "b branches[ON]"
    } else {
        "b branches"
    };
    frame.render_widget(
        Paragraph::new(format!(
            "n new  ↑/k ↓/j navigate  Enter open  / filter  s sort ({:?})  {branches_hint}  p providers  P presets  u personas  x delete  r rename  ? help  q quit",
            app.sort
        ))
        .style(Style::default().fg(app.theme.muted)),
        chunks[4],
    );
}

fn render_chat(frame: &mut Frame<'_>, app: &mut App) {
    let entry_height = (app.composer.lines().count() as u16 + 2).clamp(3, 7);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(entry_height),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(frame.area());
    let Some(history) = &app.history else { return };
    let summary = app
        .sessions
        .iter()
        .find(|session| session.session_id == history.session.session_id);
    let session_label = summary
        .map(|session| session.display_name.as_str())
        .unwrap_or("Session");
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "STcli",
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "  {session_label} · {}",
                history.session.session_id
            )),
        ]))
        .block(Block::default().borders(Borders::BOTTOM)),
        chunks[0],
    );

    let (text, message_ranges) = chat_text(app);
    let viewport = chunks[1].height.saturating_sub(2);
    let content_width = usize::from(chunks[1].width.saturating_sub(2).max(1));
    let wrapped_height = |end: usize| {
        text.lines[..end]
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.width())
                    .sum::<usize>()
                    .max(1)
                    .div_ceil(content_width)
            })
            .sum::<usize>() as u16
    };
    let content_height = wrapped_height(text.lines.len());
    let message_ranges = message_ranges
        .into_iter()
        .map(|(index, start, end, action)| {
            (
                index,
                wrapped_height(usize::from(start)),
                wrapped_height(usize::from(end)),
                action,
            )
        })
        .collect::<Vec<_>>();
    let max_scroll = content_height.saturating_sub(viewport);
    if app.follow || app.scroll == u16::MAX {
        app.scroll = max_scroll;
        app.follow = true;
    } else {
        app.scroll = app.scroll.min(max_scroll);
        if app.scroll == max_scroll {
            app.follow = true;
        }
    }
    frame.render_widget(
        Paragraph::new(text)
            .scroll((app.scroll, 0))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::LEFT | Borders::RIGHT)),
        chunks[1],
    );
    for (index, start, end, action) in message_ranges {
        let visible_start = start.saturating_sub(app.scroll);
        let visible_end = end.saturating_sub(app.scroll);
        if visible_start < viewport && end > app.scroll {
            let y = chunks[1].y + 1 + visible_start;
            let height = visible_end
                .saturating_sub(visible_start)
                .max(1)
                .min(viewport.saturating_sub(visible_start));
            app.hit_targets.push(HitTarget {
                x: chunks[1].x + 1,
                y,
                width: chunks[1].width.saturating_sub(2),
                height,
                action: HitAction::Message(index),
            });
            if let Some(action) = action {
                let previous = match action {
                    HitAction::CandidateNext => HitAction::CandidatePrevious,
                    HitAction::GreetingNext => HitAction::GreetingPrevious,
                    _ => action.clone(),
                };
                let control_y = y.saturating_add(1);
                app.hit_targets.push(HitTarget {
                    x: chunks[1].x + 1,
                    y: control_y,
                    width: 3,
                    height: 1,
                    action: previous,
                });
                app.hit_targets.push(HitTarget {
                    x: chunks[1].x + 4,
                    y: control_y,
                    width: 16.min(chunks[1].width.saturating_sub(5)),
                    height: 1,
                    action,
                });
            }
        }
    }

    let entry_text = if app.composer.is_empty() {
        "Type a message…"
    } else {
        app.composer.as_str()
    };
    frame.render_widget(
        Paragraph::new(entry_text)
            .wrap(Wrap { trim: false })
            .style(
                Style::default()
                    .fg(if app.composer.is_empty() {
                        app.theme.muted
                    } else {
                        app.theme.foreground
                    })
                    .bg(if app.chat_focus == ChatFocus::Composer {
                        app.theme.selection
                    } else {
                        app.theme.background
                    }),
            )
            .block(Block::default().borders(Borders::ALL).title(
                if app.chat_focus == ChatFocus::Composer {
                    " Message · editing "
                } else {
                    " Message · Enter to compose/respond "
                },
            )),
        chunks[2],
    );
    if app.chat_focus == ChatFocus::Composer {
        frame.set_cursor_position((
            chunks[2].x + 1 + app.composer.lines().last().map(str::len).unwrap_or(0) as u16,
            chunks[2].y + 1 + app.composer.lines().count().saturating_sub(1) as u16,
        ));
    }
    app.hit_targets.push(HitTarget {
        x: chunks[2].x,
        y: chunks[2].y,
        width: chunks[2].width,
        height: chunks[2].height,
        action: HitAction::Composer,
    });

    let provider = &history.configuration.configuration.provider;
    let provider_label = app
        .config
        .core
        .providers
        .iter()
        .find(|(_, configured)| *configured == provider)
        .map(|(name, _)| name.as_str())
        .unwrap_or(provider.id.as_str());
    let branch_position = app
        .engine
        .inspect(stcli_core::EngineQuery::Branches {
            session_id: history.session.session_id,
        })
        .ok()
        .and_then(|inspection| match inspection {
            stcli_core::EngineInspection::Branches(rows) => rows
                .iter()
                .position(|row| row.branch_id == history.branch.branch_id)
                .map(|index| (index + 1, rows.len())),
            _ => None,
        })
        .unwrap_or((1, 1));
    let preset_label = history
        .configuration
        .configuration
        .prompt_preset_revision
        .as_ref()
        .map(ToString::to_string)
        .map(|hash| short_hash(&hash).to_owned())
        .unwrap_or_else(|| "none".to_owned());
    let persona_label = summary
        .map(|session| session.persona_label.as_str())
        .unwrap_or(history.configuration.configuration.persona_name.as_str());
    frame.render_widget(
        Paragraph::new(format!(
            "{session_label} / {persona_label} · {provider_label}:{} · branch {}/{} · preset {}",
            provider.model, branch_position.0, branch_position.1, preset_label
        ))
        .style(Style::default().fg(app.theme.muted)),
        chunks[3],
    );
    let hints = if app.generation.is_some() {
        "Esc stop  Ctrl+C quit (confirm)  ↑/↓ scroll  ? help".to_owned()
    } else if app.chat_focus == ChatFocus::Composer {
        "Enter send/respond  Shift+Enter newline  ↑/Esc/Tab history  Ctrl+C quit".to_owned()
    } else {
        let mut hints =
            "Enter compose/respond  ↑/k ↓/j scroll  ←/→ select  x delete  b branch  p provider  P preset  c copy"
                .to_owned();
        if history.turns.last().is_some() {
            hints.push_str("  r regenerate");
        }
        if history
            .turns
            .last()
            .is_some_and(|turn| turn.turn.selected_candidate_id.is_some())
        {
            hints.push_str("  e continue");
        }
        hints.push_str("  ? help  q quit");
        hints
    };
    let regenerate_offset = hints.find("r regenerate");
    let continue_offset = hints.find("e continue");
    frame.render_widget(
        Paragraph::new(hints).style(Style::default().fg(app.theme.muted)),
        chunks[4],
    );
    if app.generation.is_some() {
        app.hit_targets.push(HitTarget {
            x: chunks[4].x,
            y: chunks[4].y,
            width: 8,
            height: 1,
            action: HitAction::Stop,
        });
    } else if app.chat_focus == ChatFocus::History {
        if let Some(offset) = regenerate_offset {
            app.hit_targets.push(HitTarget {
                x: chunks[4].x.saturating_add(offset as u16),
                y: chunks[4].y,
                width: "r regenerate".len() as u16,
                height: 1,
                action: HitAction::Regenerate,
            });
        }
        if let Some(offset) = continue_offset {
            app.hit_targets.push(HitTarget {
                x: chunks[4].x.saturating_add(offset as u16),
                y: chunks[4].y,
                width: "e continue".len() as u16,
                height: 1,
                action: HitAction::Continue,
            });
        }
    }
}

type MessageRange = (usize, u16, u16, Option<HitAction>);

fn chat_text(app: &App) -> (Text<'static>, Vec<MessageRange>) {
    let history = app.history.as_ref().expect("chat view has history");
    let character_label = app
        .sessions
        .iter()
        .find(|session| session.session_id == history.session.session_id)
        .map(|session| session.character_label.as_str())
        .unwrap_or("Character");
    let mut lines = Vec::new();
    let mut ranges = Vec::new();
    let mut message_index = 0;
    if let Some(greeting) = &history.greeting {
        let start = lines.len() as u16;
        lines.push(label_line(
            &format!("[Greeting · {character_label}]"),
            app.theme.greeting,
            app.chat_focus == ChatFocus::History && app.focused_message == message_index,
            app,
        ));
        if greeting.total > 1 {
            lines.push(Line::from(Span::styled(
                format!("← {}/{} →", greeting.index + 1, greeting.total),
                Style::default().fg(app.theme.accent),
            )));
        }
        append_markdown(&mut lines, &greeting.content, app, message_index);
        lines.push(Line::from("────────────────────────────────"));
        ranges.push((
            message_index,
            start,
            lines.len() as u16,
            (greeting.total > 1).then_some(HitAction::GreetingNext),
        ));
        message_index += 1;
    }
    let mut generation_rendered = false;
    for (i, turn) in history.turns.iter().enumerate() {
        let candidate = selected_candidate(turn);
        let is_last = i == history.turns.len() - 1;
        let generating_here = is_last
            && app
                .generation
                .as_ref()
                .is_some_and(|g| turn.candidates.is_empty() || g.pending_input.is_none());

        let skip_character = turn.candidates.is_empty() && !generating_here;

        let start = lines.len() as u16;
        lines.push(label_line(
            "[You]",
            app.theme.user,
            app.chat_focus == ChatFocus::History && app.focused_message == message_index,
            app,
        ));
        append_markdown(&mut lines, &turn.turn.user_content, app, message_index);
        lines.push(Line::from("────────────────────────────────"));
        ranges.push((message_index, start, lines.len() as u16, None));
        message_index += 1;

        if skip_character {
            continue;
        }

        let start = lines.len() as u16;
        if generating_here {
            generation_rendered = true;
            let generation = app.generation.as_ref().unwrap();
            let gen_label = if generation.continues {
                format!("[{character_label} · continuing]")
            } else {
                format!("[{character_label} · generating]")
            };
            lines.push(label_line(&gen_label, app.theme.character, false, app));
            if !generation.reasoning.is_empty() {
                append_reasoning(&mut lines, &generation.reasoning, app);
            }
            if generation.continues {
                if let Some(candidate) = candidate {
                    let display = candidate
                        .rendered_content
                        .as_deref()
                        .unwrap_or(&candidate.content);
                    let content = format!("{}{}", display, generation.partial);
                    append_markdown(&mut lines, &content, app, usize::MAX);
                    if generation.partial.is_empty() {
                        lines.push(Line::from(Span::styled(
                            "…",
                            Style::default().fg(app.theme.accent),
                        )));
                    }
                }
            } else if generation.streaming {
                if generation.partial.is_empty() && generation.reasoning.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "Waiting for provider…",
                        Style::default().fg(app.theme.muted),
                    )));
                } else if !generation.partial.is_empty() {
                    append_markdown(&mut lines, &generation.partial, app, usize::MAX);
                }
            } else {
                lines.push(Line::from(Span::styled(
                    "Generating…",
                    Style::default().fg(app.theme.accent),
                )));
            }
        } else {
            let label = candidate.map_or_else(
                || format!("[{character_label}]"),
                |candidate| match candidate.origin {
                    stcli_core::CandidateOrigin::Continued => {
                        format!("[{character_label} · continued]")
                    }
                    stcli_core::CandidateOrigin::Generated => format!("[{character_label}]"),
                    stcli_core::CandidateOrigin::Manual => {
                        format!("[{character_label} · manual]")
                    }
                    stcli_core::CandidateOrigin::AcceptedPartial => {
                        format!("[{character_label} · stopped]")
                    }
                },
            );
            lines.push(label_line(
                &label,
                app.theme.character,
                app.chat_focus == ChatFocus::History && app.focused_message == message_index,
                app,
            ));
            if let Some(candidate) = candidate {
                let selected = turn
                    .candidates
                    .iter()
                    .position(|item| item.candidate_id == candidate.candidate_id)
                    .unwrap_or(0);
                lines.push(Line::from(Span::styled(
                    format!("← {}/{} →", selected + 1, turn.candidates.len()),
                    Style::default().fg(app.theme.accent),
                )));
            }
            if let Some(candidate) = candidate {
                let display = candidate
                    .rendered_content
                    .as_deref()
                    .unwrap_or(&candidate.content);
                append_markdown(&mut lines, display, app, message_index);
            } else {
                lines.push(Line::from(Span::styled(
                    "No selected Candidate",
                    Style::default().fg(app.theme.muted),
                )));
            }
        }
        lines.push(Line::from("────────────────────────────────"));
        ranges.push((
            message_index,
            start,
            lines.len() as u16,
            if generating_here {
                None
            } else {
                candidate.map(|_| HitAction::CandidateNext)
            },
        ));
        message_index += 1;
    }
    if let Some(generation) = &app.generation
        && !generation_rendered
    {
        lines.push(label_line(
            &format!("[{character_label} · generating]"),
            app.theme.character,
            false,
            app,
        ));
        if !generation.reasoning.is_empty() {
            append_reasoning(&mut lines, &generation.reasoning, app);
        }
        if generation.streaming {
            if generation.partial.is_empty() && generation.reasoning.is_empty() {
                lines.push(Line::from(Span::styled(
                    "Waiting for provider…",
                    Style::default().fg(app.theme.muted),
                )));
            } else if !generation.partial.is_empty() {
                append_markdown(&mut lines, &generation.partial, app, usize::MAX);
            }
        } else {
            lines.push(Line::from(Span::styled(
                "Generating…",
                Style::default().fg(app.theme.accent),
            )));
        }
    }
    (Text::from(lines), ranges)
}

fn label_line(
    label: &str,
    color: ratatui::style::Color,
    focused: bool,
    app: &App,
) -> Line<'static> {
    Line::from(Span::styled(
        label.to_owned(),
        Style::default()
            .fg(color)
            .bg(if focused {
                app.theme.selection
            } else {
                app.theme.background
            })
            .add_modifier(Modifier::BOLD),
    ))
}

fn append_markdown(lines: &mut Vec<Line<'static>>, source: &str, app: &App, index: usize) {
    let mut text = markdown::render(source, app.theme);
    if app.chat_focus == ChatFocus::History && app.focused_message == index {
        for line in &mut text.lines {
            for span in &mut line.spans {
                span.style = span.style.bg(app.theme.selection);
            }
        }
    }
    lines.extend(text.lines);
}

fn append_reasoning(lines: &mut Vec<Line<'static>>, source: &str, app: &App) {
    let mut text = markdown::render(source, app.theme);
    for line in &mut text.lines {
        for span in &mut line.spans {
            span.style = span
                .style
                .fg(app.theme.muted)
                .add_modifier(Modifier::ITALIC);
        }
    }
    lines.extend(text.lines);
}

fn render_popup(frame: &mut Frame<'_>, app: &mut App) {
    let popup = app.popup.as_ref().expect("popup exists");
    let area = centered(frame.area(), 70, 70);
    match popup {
        Popup::Help => {
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new("Sessions\n  n  new session · ↑/↓ or j/k  navigate\n  /  filter · s  sort · Enter  open\n  b  toggle branch tree · x  delete · r  rename\n  p  providers · P  presets · u  personas\n\nChat composer\n  Enter  send or answer an unanswered user message · Shift+Enter  newline\n  Escape or Tab  focus history\n\nChat history\n  ↑/↓ or j/k  scroll · Tab  focus next message · c  copy\n  ←/→  select Greeting or Candidate\n  x  delete candidate (on user message: delete turn)\n  r  regenerate · e  continue · Enter  compose or answer selected user message\n  b  Branches · p  providers · P  presets\n\nEvery action is available without a mouse. Escape closes this help.")
                    .wrap(Wrap { trim: false })
                    .block(Block::default().borders(Borders::ALL).title(" Help "))
                    .style(Style::default().bg(app.theme.background).fg(app.theme.foreground)),
                area,
            );
        }
        Popup::Rename { input, .. } => {
            let rename_area = centered(frame.area(), 60, 5);
            frame.render_widget(Clear, rename_area);
            frame.render_widget(
                Paragraph::new(input.as_str())
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Rename session · Enter confirm · Esc cancel "),
                    )
                    .style(
                        Style::default()
                            .bg(app.theme.background)
                            .fg(app.theme.foreground),
                    ),
                rename_area,
            );
            frame.set_cursor_position((rename_area.x + 1 + input.len() as u16, rename_area.y + 1));
        }
        Popup::ConfirmExit => {
            let confirm_area = centered(frame.area(), 52, 5);
            frame.render_widget(Clear, confirm_area);
            frame.render_widget(
                Paragraph::new("Generation is active. Stop it and exit? [y/N]")
                    .alignment(Alignment::Center)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Confirm exit "),
                    )
                    .style(
                        Style::default()
                            .bg(app.theme.background)
                            .fg(app.theme.foreground),
                    ),
                confirm_area,
            );
        }
        Popup::ConfirmDelete { name, .. } => {
            let confirm_area = centered(frame.area(), 70, 5);
            frame.render_widget(Clear, confirm_area);
            frame.render_widget(
                Paragraph::new(format!(
                    "Purge session {name}? This cannot be undone. [y/N]"
                ))
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Confirm purge "),
                )
                .style(
                    Style::default()
                        .bg(app.theme.background)
                        .fg(app.theme.foreground),
                ),
                confirm_area,
            );
        }
        Popup::ConfirmDeleteProvider { name, .. } => {
            let confirm_area = centered(frame.area(), 70, 5);
            frame.render_widget(Clear, confirm_area);
            frame.render_widget(
                Paragraph::new(format!("Delete provider profile '{name}'? [y/N]"))
                    .alignment(Alignment::Center)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Confirm provider deletion "),
                    )
                    .style(
                        Style::default()
                            .bg(app.theme.background)
                            .fg(app.theme.foreground),
                    ),
                confirm_area,
            );
        }
        Popup::Branches { rows, selected } => {
            let current = app.history.as_ref().map(|history| history.branch.branch_id);
            let labels = rows
                .iter()
                .map(|branch| {
                    format!(
                        "{}{}  parent {}  fork {}",
                        if Some(branch.branch_id) == current {
                            "* "
                        } else {
                            "  "
                        },
                        branch.branch_id,
                        branch
                            .parent_branch_id
                            .map(|id| id.to_string())
                            .unwrap_or_else(|| "root".to_owned()),
                        branch
                            .forked_from_turn_id
                            .map(|id| id.to_string())
                            .unwrap_or_else(|| "start".to_owned())
                    )
                })
                .collect();
            render_list_popup(
                frame,
                app,
                area,
                " Branches · * current ",
                labels,
                *selected,
                None,
            );
        }
        Popup::Providers {
            names, selected, ..
        } => {
            let provider_height = frame.area().height.saturating_mul(70).saturating_div(100);
            let provider_area = centered(frame.area(), 50, provider_height);
            let current = app
                .history
                .as_ref()
                .map(|history| &history.configuration.configuration.provider);
            let mut labels: Vec<String> = names
                .iter()
                .map(|name| {
                    let provider = &app.config.core.providers[name];
                    format!(
                        "{}{name}  {}  {}",
                        if Some(provider) == current {
                            "* "
                        } else {
                            "  "
                        },
                        provider.model,
                        provider.base_url
                    )
                })
                .collect();
            labels.push("+ Add new profile...".to_owned());
            render_list_popup(
                frame,
                app,
                provider_area,
                " Provider profiles · * current · Enter switch · c copy · e edit · x delete · a add · Esc close ",
                labels,
                *selected,
                None,
            );
        }
        Popup::Presets(state) => {
            let rows = state.filtered_rows();
            let selected = state.selected;
            let current = app.history.as_ref().and_then(|history| {
                history
                    .configuration
                    .configuration
                    .prompt_preset_revision
                    .as_ref()
            });
            let mut labels = vec![format!(
                "{}No preset",
                if current.is_none() { "* " } else { "  " }
            )];
            labels.extend(rows.iter().map(|row| {
                format!(
                    "{}{}  {}",
                    if Some(&row.record.revision_hash) == current {
                        "* "
                    } else {
                        "  "
                    },
                    row.label,
                    short_hash(&row.record.revision_hash.to_string())
                )
            }));
            let title = if state.filtering {
                format!(" Filter: {}█ ", state.filter)
            } else {
                " Prompt presets ".to_owned()
            };
            if state.show_details {
                let detail = selected
                    .checked_sub(1)
                    .and_then(|index| rows.get(index))
                    .map(|row| (*row).clone());
                let preset_area =
                    centered(frame.area(), 94, frame.area().height.saturating_mul(3) / 4);
                frame.render_widget(Clear, preset_area);
                let body_and_footer = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(1), Constraint::Length(1)])
                    .split(preset_area);
                let columns = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
                    .split(body_and_footer[0]);
                render_list_popup(frame, app, columns[0], &title, labels, selected, None);
                render_preset_details(frame, app, columns[1], detail.as_ref());
                frame.render_widget(
                    Paragraph::new(
                        "Enter select · c copy · i import · d details · / filter · Esc close",
                    )
                    .style(
                        Style::default()
                            .bg(app.theme.background)
                            .fg(app.theme.muted),
                    ),
                    body_and_footer[1],
                );
            } else {
                render_list_popup(
                    frame,
                    app,
                    area,
                    &title,
                    labels,
                    selected,
                    Some("Enter select · c copy · i import · d details · / filter · Esc close"),
                );
            }
        }
        Popup::ImportArtifact(state) => {
            let import_area = centered(frame.area(), 68, 7);
            frame.render_widget(Clear, import_area);
            let kind = state
                .expected_kind
                .map_or("Character Card", |expected| match expected {
                    stcli_core::ArtifactKind::ChatCompletionPreset => "Prompt Preset",
                    _ => "Character Card",
                });
            let support =
                if state.expected_kind == Some(stcli_core::ArtifactKind::ChatCompletionPreset) {
                    "Supported: .json prompt presets (supports ~/ paths)"
                } else {
                    "Supported: .json, .png, .webp, and .charx artifacts (supports ~/ paths)"
                };
            let block = Block::default().borders(Borders::ALL).title(format!(
                " Import {kind} from File · Enter import · Esc cancel "
            ));
            let content = vec![
                Line::from(vec![
                    Span::styled(
                        "Path: ",
                        Style::default()
                            .fg(app.theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(&state.input, Style::default().fg(app.theme.foreground)),
                    Span::styled("█", Style::default().fg(app.theme.accent)),
                ]),
                Line::from(""),
                Line::from(vec![Span::styled(
                    support,
                    Style::default().fg(app.theme.muted),
                )]),
            ];
            frame.render_widget(
                Paragraph::new(content).block(block).style(
                    Style::default()
                        .bg(app.theme.background)
                        .fg(app.theme.foreground),
                ),
                import_area,
            );
        }
        Popup::Personas(state) => {
            let height = frame.area().height.saturating_mul(70).saturating_div(100);
            let persona_area = centered(frame.area(), 72, height);
            let labels = state
                .personas
                .iter()
                .map(|persona| {
                    if persona.description.is_empty() {
                        persona.name.clone()
                    } else {
                        format!(
                            "{}  {}",
                            persona.name,
                            truncate_display(&persona.description, 42)
                        )
                    }
                })
                .collect();
            render_list_popup(
                frame,
                app,
                persona_area,
                " Personas ",
                labels,
                state.selected,
                Some("Enter select · a add · c copy · e edit · x delete · i import · Esc close"),
            );
        }
        Popup::PersonaEditor(state) => {
            let editor_area = centered(frame.area(), 76, 11);
            frame.render_widget(Clear, editor_area);
            let field_style = |index: usize| {
                if state.focused_field == index {
                    Style::default()
                        .fg(app.theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.foreground)
                }
            };
            let marker = |index: usize| {
                if state.focused_field == index {
                    "> "
                } else {
                    "  "
                }
            };
            let content = vec![
                Line::from(vec![
                    Span::styled(format!("{}Name: ", marker(0)), field_style(0)),
                    Span::styled(&state.name, field_style(0)),
                ]),
                Line::from(vec![
                    Span::styled(format!("{}Description: ", marker(1)), field_style(1)),
                    Span::styled(&state.description, field_style(1)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled(format!("{}[Save]", marker(2)), field_style(2)),
                    Span::raw("    "),
                    Span::styled(format!("{}[Cancel]", marker(3)), field_style(3)),
                ]),
            ];
            frame.render_widget(
                Paragraph::new(content)
                    .wrap(Wrap { trim: false })
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Persona · Ctrl+S save · Esc cancel "),
                    )
                    .style(
                        Style::default()
                            .bg(app.theme.background)
                            .fg(app.theme.foreground),
                    ),
                editor_area,
            );
        }
        Popup::ImportPersonas(state) => {
            let import_area = centered(frame.area(), 72, 7);
            frame.render_widget(Clear, import_area);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::styled("Path: ", Style::default().fg(app.theme.accent)),
                        Span::styled(&state.input, Style::default().fg(app.theme.foreground)),
                        Span::styled("█", Style::default().fg(app.theme.accent)),
                    ]),
                    Line::from(""),
                    Line::from(Span::styled(
                        "SillyTavern personas_*.json or personas.json",
                        Style::default().fg(app.theme.muted),
                    )),
                ])
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Import Personas · Enter import · Esc cancel "),
                )
                .style(
                    Style::default()
                        .bg(app.theme.background)
                        .fg(app.theme.foreground),
                ),
                import_area,
            );
        }
        Popup::ClonePreset(state) => {
            let clone_area = centered(frame.area(), 72, 13);
            frame.render_widget(Clear, clone_area);
            let field_style = |index: usize| {
                if state.focused_field == index {
                    Style::default()
                        .fg(app.theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.foreground)
                }
            };
            let marker = |index: usize| {
                if state.focused_field == index {
                    "> "
                } else {
                    "  "
                }
            };
            let content = vec![
                Line::from(vec![
                    Span::styled(format!("{}New Preset Name: ", marker(0)), field_style(0)),
                    Span::styled(&state.name, field_style(0)),
                ]),
                Line::from(vec![
                    Span::styled(format!("{}Temperature: ", marker(1)), field_style(1)),
                    Span::styled(&state.temperature, field_style(1)),
                ]),
                Line::from(vec![
                    Span::styled(format!("{}Max Context Tokens: ", marker(2)), field_style(2)),
                    Span::styled(&state.max_context, field_style(2)),
                ]),
                Line::from(vec![
                    Span::styled(format!("{}Max Tokens: ", marker(3)), field_style(3)),
                    Span::styled(&state.max_tokens, field_style(3)),
                ]),
                Line::from(Span::styled(
                    format!(
                        "{}System Prompt: {}",
                        marker(4),
                        if state.use_sysprompt { "On" } else { "Off" }
                    ),
                    field_style(4),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled(format!("{}[Save]", marker(5)), field_style(5)),
                    Span::raw("    "),
                    Span::styled(format!("{}[Cancel]", marker(6)), field_style(6)),
                ]),
            ];
            frame.render_widget(
                Paragraph::new(content)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Clone Prompt Preset · Ctrl+S save · Esc cancel "),
                    )
                    .style(
                        Style::default()
                            .bg(app.theme.background)
                            .fg(app.theme.foreground),
                    ),
                clone_area,
            );
        }
        Popup::ProviderProfile(state) => {
            let profile_area = centered(frame.area(), 72, 20);
            frame.render_widget(Clear, profile_area);
            let template_name = if state.selected_template == 0 {
                "Custom".to_owned()
            } else if let Some(t) = state.templates.get(state.selected_template - 1) {
                format!("{} ({})", t.name, t.id)
            } else {
                "Custom".to_owned()
            };
            let env_status = if state.api_key_env.trim().is_empty() {
                Span::styled(
                    " (no key required/set)",
                    Style::default().fg(app.theme.muted),
                )
            } else if std::env::var(state.api_key_env.trim()).is_ok() {
                Span::styled(" [set]", Style::default().fg(Color::Green))
            } else {
                Span::styled(" [not set]", Style::default().fg(Color::Yellow))
            };

            let field_style = |idx: usize| {
                if state.focused_field == idx {
                    Style::default()
                        .fg(app.theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.foreground)
                }
            };
            let cursor = |idx: usize| {
                if state.focused_field == idx {
                    "█"
                } else {
                    ""
                }
            };
            let name = cursor_parts(&state.name, state.focused_field == 1, state.cursor_position);
            let base_url = cursor_parts(
                &state.base_url,
                state.focused_field == 2,
                state.cursor_position,
            );
            let model = cursor_parts(
                &state.model,
                state.focused_field == 3,
                state.cursor_position,
            );
            let chat_path = cursor_parts(
                &state.chat_path,
                state.focused_field == 4,
                state.cursor_position,
            );
            let api_key_env = cursor_parts(
                &state.api_key_env,
                state.focused_field == 5,
                state.cursor_position,
            );
            let timeout = cursor_parts(
                &state.timeout_seconds,
                state.focused_field == 7,
                state.cursor_position,
            );

            let lines = vec![
                Line::from(vec![
                    Span::styled(
                        if state.focused_field == 0 {
                            "> Template:    < "
                        } else {
                            "  Template:    < "
                        },
                        field_style(0),
                    ),
                    Span::styled(template_name, field_style(0)),
                    Span::styled(
                        " >  (Space/←/→ to cycle)",
                        Style::default().fg(app.theme.muted),
                    ),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled(
                        if state.focused_field == 1 {
                            "> Name:        "
                        } else {
                            "  Name:        "
                        },
                        field_style(1),
                    ),
                    Span::styled(name.0, field_style(1)),
                    Span::styled(cursor(1), Style::default().fg(app.theme.accent)),
                    Span::styled(name.1, field_style(1)),
                ]),
                Line::from(vec![
                    Span::styled(
                        if state.focused_field == 2 {
                            "> Base URL:    "
                        } else {
                            "  Base URL:    "
                        },
                        field_style(2),
                    ),
                    Span::styled(base_url.0, field_style(2)),
                    Span::styled(cursor(2), Style::default().fg(app.theme.accent)),
                    Span::styled(base_url.1, field_style(2)),
                ]),
                Line::from(vec![
                    Span::styled(
                        if state.focused_field == 3 {
                            "> Model:       "
                        } else {
                            "  Model:       "
                        },
                        field_style(3),
                    ),
                    Span::styled(model.0, field_style(3)),
                    Span::styled(cursor(3), Style::default().fg(app.theme.accent)),
                    Span::styled(model.1, field_style(3)),
                ]),
                Line::from(vec![
                    Span::styled(
                        if state.focused_field == 4 {
                            "> Chat Path:   "
                        } else {
                            "  Chat Path:   "
                        },
                        field_style(4),
                    ),
                    Span::styled(chat_path.0, field_style(4)),
                    Span::styled(cursor(4), Style::default().fg(app.theme.accent)),
                    Span::styled(chat_path.1, field_style(4)),
                ]),
                Line::from(vec![
                    Span::styled(
                        if state.focused_field == 5 {
                            "> API Key Env: "
                        } else {
                            "  API Key Env: "
                        },
                        field_style(5),
                    ),
                    Span::styled(api_key_env.0, field_style(5)),
                    Span::styled(cursor(5), Style::default().fg(app.theme.accent)),
                    Span::styled(api_key_env.1, field_style(5)),
                    env_status,
                ]),
                Line::from(vec![
                    Span::styled(
                        if state.focused_field == 6 {
                            "> Stream:      < "
                        } else {
                            "  Stream:      < "
                        },
                        field_style(6),
                    ),
                    Span::styled(if state.stream { "true" } else { "false" }, field_style(6)),
                    Span::styled(
                        " >  (Space to toggle)",
                        Style::default().fg(app.theme.muted),
                    ),
                ]),
                Line::from(vec![
                    Span::styled(
                        if state.focused_field == 7 {
                            "> Timeout:     "
                        } else {
                            "  Timeout:     "
                        },
                        field_style(7),
                    ),
                    Span::styled(timeout.0, field_style(7)),
                    Span::styled(cursor(7), Style::default().fg(app.theme.accent)),
                    Span::styled(timeout.1, field_style(7)),
                    Span::styled(" seconds", Style::default().fg(app.theme.muted)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        if state.focused_field == 8 {
                            "[ Save Profile ]"
                        } else {
                            "  Save Profile  "
                        },
                        if state.focused_field == 8 {
                            Style::default()
                                .bg(app.theme.accent)
                                .fg(app.theme.background)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(app.theme.muted)
                        },
                    ),
                    Span::raw("     "),
                    Span::styled(
                        if state.focused_field == 9 {
                            "[ Cancel ]"
                        } else {
                            "  Cancel  "
                        },
                        if state.focused_field == 9 {
                            Style::default()
                                .bg(app.theme.accent)
                                .fg(app.theme.background)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(app.theme.muted)
                        },
                    ),
                ]),
                Line::from(""),
                Line::from(vec![Span::styled(
                    "Tab/↓ next · Shift+Tab/↑ prev · Text ←/→ move · Space cycles · Enter/Ctrl+S save · Esc cancel",
                    Style::default().fg(app.theme.muted),
                )]),
            ];

            frame.render_widget(
                Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title(
                        if state.original_name.is_some() {
                            " Edit Provider Profile "
                        } else {
                            " New Provider Profile "
                        },
                    ))
                    .style(
                        Style::default()
                            .bg(app.theme.background)
                            .fg(app.theme.foreground),
                    ),
                profile_area,
            );
        }
        Popup::NewSession(state) => {
            let session_area = centered(frame.area(), 72, 18);
            frame.render_widget(Clear, session_area);

            let char_label = if state.characters.is_empty() {
                "<No characters - Press Enter to import from file>".to_owned()
            } else if state.selected_character == state.characters.len() {
                "<Import from file...>".to_owned()
            } else {
                let c = &state.characters[state.selected_character];
                format!(
                    "<{} ({} greeting{})>",
                    c.name,
                    c.greeting_count,
                    if c.greeting_count == 1 { "" } else { "s" }
                )
            };

            let prov_label = if state.providers.is_empty() {
                "<No providers - Press Enter to add profile>".to_owned()
            } else if state.selected_provider == state.providers.len() {
                "<+ Add new profile...>".to_owned()
            } else {
                format!("<{}>", state.providers[state.selected_provider])
            };

            let preset_label = if state.selected_preset == 0 {
                "<None>".to_owned()
            } else if let Some(p) = state.presets.get(state.selected_preset - 1) {
                format!("<{}>", p.label)
            } else {
                "<Import preset...>".to_owned()
            };

            let persona_label = if state.selected_persona < state.personas.len() {
                format!("<{}>", state.personas[state.selected_persona].name)
            } else if state.selected_persona == state.personas.len() {
                "<+ Add new persona...>".to_owned()
            } else {
                "<[Edit persona...]>".to_owned()
            };

            let greeting_label = {
                let total = state
                    .characters
                    .get(state.selected_character)
                    .map(|c| c.greeting_count)
                    .unwrap_or(1);
                format!("<Greeting {} of {}>", state.selected_greeting + 1, total)
            };

            let field_style = |idx: usize| {
                if state.focused_field == idx {
                    Style::default()
                        .fg(app.theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.foreground)
                }
            };

            let lines = vec![
                Line::from(vec![
                    Span::styled(
                        if state.focused_field == 0 {
                            "> Character:    "
                        } else {
                            "  Character:    "
                        },
                        field_style(0),
                    ),
                    Span::styled(char_label, field_style(0)),
                ]),
                Line::from(vec![
                    Span::styled(
                        if state.focused_field == 1 {
                            "> Provider:     "
                        } else {
                            "  Provider:     "
                        },
                        field_style(1),
                    ),
                    Span::styled(prov_label, field_style(1)),
                ]),
                Line::from(vec![
                    Span::styled(
                        if state.focused_field == 2 {
                            "> Preset:       "
                        } else {
                            "  Preset:       "
                        },
                        field_style(2),
                    ),
                    Span::styled(preset_label, field_style(2)),
                ]),
                Line::from(vec![
                    Span::styled(
                        if state.focused_field == 3 {
                            "> Persona:      "
                        } else {
                            "  Persona:      "
                        },
                        field_style(3),
                    ),
                    Span::styled(persona_label, field_style(3)),
                ]),
                Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        truncate_display(&state.persona_description, 58),
                        Style::default().fg(app.theme.muted),
                    ),
                ]),
                Line::from(vec![
                    Span::styled(
                        if state.focused_field == 4 {
                            "> Greeting:     "
                        } else {
                            "  Greeting:     "
                        },
                        field_style(4),
                    ),
                    Span::styled(greeting_label, field_style(4)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        if state.focused_field == 5 {
                            "[ Create Session ]"
                        } else {
                            "  Create Session  "
                        },
                        if state.focused_field == 5 {
                            Style::default()
                                .bg(app.theme.accent)
                                .fg(app.theme.background)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(app.theme.muted)
                        },
                    ),
                    Span::raw("     "),
                    Span::styled(
                        if state.focused_field == 6 {
                            "[ Cancel ]"
                        } else {
                            "  Cancel  "
                        },
                        if state.focused_field == 6 {
                            Style::default()
                                .bg(app.theme.accent)
                                .fg(app.theme.background)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(app.theme.muted)
                        },
                    ),
                ]),
                Line::from(""),
                Line::from(vec![Span::styled(
                    "Tab/↓ next · Shift+Tab/↑ prev · Space/←/→ cycle · Enter/Ctrl+S create · Esc cancel",
                    Style::default().fg(app.theme.muted),
                )]),
            ];

            frame.render_widget(
                Paragraph::new(lines)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Create New Session "),
                    )
                    .style(
                        Style::default()
                            .bg(app.theme.background)
                            .fg(app.theme.foreground),
                    ),
                session_area,
            );
        }
    }
}

fn render_list_popup(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    title: &str,
    labels: Vec<String>,
    selected: usize,
    footer: Option<&str>,
) {
    frame.render_widget(Clear, area);
    let visible = area.height.saturating_sub(2) as usize;
    let window_start = selected
        .saturating_add(1)
        .saturating_sub(visible)
        .min(labels.len().saturating_sub(visible));
    let items = labels
        .iter()
        .enumerate()
        .skip(window_start)
        .take(visible)
        .map(|(index, label)| {
            ListItem::new(label.clone()).style(if index == selected {
                Style::default()
                    .bg(app.theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            })
        });
    let mut block = Block::default().borders(Borders::ALL).title(title);
    if let Some(footer) = footer {
        block = block.title_bottom(footer);
    }
    frame.render_widget(
        List::new(items).block(block).style(
            Style::default()
                .bg(app.theme.background)
                .fg(app.theme.foreground),
        ),
        area,
    );
    for row in 0..labels.len().saturating_sub(window_start).min(visible) {
        app.hit_targets.push(HitTarget {
            x: area.x + 1,
            y: area.y + 1 + row as u16,
            width: area.width.saturating_sub(2),
            height: 1,
            action: HitAction::PopupRow(window_start + row),
        });
    }
}

fn render_preset_details(
    frame: &mut Frame<'_>,
    app: &App,
    area: Rect,
    preset: Option<&crate::app::PresetOption>,
) {
    let mut lines = Vec::new();
    if let Some(preset) = preset {
        let summary = &preset.summary;
        lines.extend([
            Line::from(Span::styled(
                &preset.label,
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "General Settings",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(format!("Prompts: {}", summary.prompt_count)),
            Line::from(format!("Order profile: {}", summary.order_profile)),
            Line::from(format!(
                "System prompt: {}",
                if summary.system_prompt_enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Prompt Ordering",
                Style::default().add_modifier(Modifier::BOLD),
            )),
        ]);
        lines.extend(
            summary
                .prompt_order
                .iter()
                .enumerate()
                .map(|(index, prompt)| Line::from(format!("{}. {prompt}", index + 1))),
        );
        lines.extend([
            Line::from(""),
            Line::from(Span::styled(
                "Generation Parameters",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(format!(
                "Temperature: {}",
                summary.temperature.as_deref().unwrap_or("—")
            )),
            Line::from(format!(
                "top_p: {}",
                summary.top_p.as_deref().unwrap_or("—")
            )),
            Line::from(format!(
                "max_tokens: {}",
                summary.max_tokens.as_deref().unwrap_or("—")
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Embedded Scripts",
                Style::default().add_modifier(Modifier::BOLD),
            )),
        ]);
        if summary.scripts.is_empty() {
            lines.push(Line::from("None"));
        } else {
            lines.extend(summary.scripts.iter().map(|script| {
                Line::from(format!(
                    "{} · {} · {} [inert — requires grant]",
                    script.name,
                    if script.placement.is_empty() {
                        "Unknown"
                    } else {
                        &script.placement
                    },
                    script.digest
                ))
            }));
        }
    } else {
        lines.push(Line::from("No preset selected"));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Details "))
            .wrap(Wrap { trim: false })
            .style(
                Style::default()
                    .bg(app.theme.background)
                    .fg(app.theme.foreground),
            ),
        area,
    );
}

fn centered(area: Rect, percent_x: u16, height: u16) -> Rect {
    let width = area
        .width
        .saturating_mul(percent_x)
        .saturating_div(100)
        .max(1);
    let height = height.min(area.height).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn format_date(milliseconds: u64) -> String {
    chrono::DateTime::from_timestamp_millis(milliseconds as i64)
        .map(|utc| {
            utc.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| format!("unix:{}", milliseconds / 1000))
}

fn truncate_display(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut result: String = text.chars().take(max.saturating_sub(1)).collect();
        result.push('…');
        result
    }
}

fn short_id(id: &str) -> &str {
    id.get(id.len().saturating_sub(8)..).unwrap_or(id)
}

fn short_hash(hash: &str) -> &str {
    hash.get(hash.len().saturating_sub(12)..).unwrap_or(hash)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::App, config::Config};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};
    use stcli_core::StcliEngine;
    #[tokio::test]
    async fn reasoning_is_dimmed_while_streaming_and_hidden_after_generation() {
        let provider = stcli_testkit::MockProvider::spawn(["Final response"])
            .await
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        let character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        let mut configuration = stcli_testkit::configuration(character.revision_hash);
        configuration.provider = provider.provider_settings();
        let created = store.create_session(configuration, 0).unwrap();
        store
            .send_message(
                created.session.session_id,
                created.branch.branch_id,
                "Hello".to_owned(),
                |_| {},
            )
            .await
            .unwrap();
        drop(store);
        let mut app = App::load(
            StcliEngine::new(database),
            Config::default(),
            Some(created.session.session_id),
        )
        .unwrap();
        app.generation = Some(crate::app::GenerationState {
            partial: String::new(),
            reasoning: "Thinking live".to_owned(),
            streaming: true,
            pending_input: None,
            continues: false,
        });

        let (active, _) = chat_text(&app);
        let reasoning_span = active
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == "Thinking live")
            .unwrap();
        assert_eq!(reasoning_span.style.fg, Some(app.theme.muted));
        assert!(reasoning_span.style.add_modifier.contains(Modifier::ITALIC));

        app.finish_generation(Ok(stcli_core::EngineResult::DeletedTurn(
            stcli_core::DeletionReceipt {
                entity_id: stcli_core::EntityId::new(),
                deleted: false,
            },
        )));
        let (finished, _) = chat_text(&app);
        let finished_text = finished
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(finished_text.contains("Final response"));
        assert!(!finished_text.contains("Thinking live"));
        provider.shutdown().await;
    }

    #[test]
    fn final_branch_uses_end_connector() {
        // Regression test for issue 16: the final branch closes its session tree.
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        let character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        let created = store
            .create_session(stcli_testkit::configuration(character.revision_hash), 0)
            .unwrap();
        let child = store
            .create_branch(
                created.session.session_id,
                created.branch.branch_id,
                created.branch.greeting_index,
            )
            .unwrap();
        drop(store);
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        app.show_branches = true;
        app.reload_sessions().unwrap();
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains(&format!(
            "├─ {}",
            short_id(&created.branch.branch_id.to_string())
        )));
        assert!(rendered.contains(&format!("└─ {}", short_id(&child.branch_id.to_string()))));
    }
    #[test]
    fn delete_confirmation_only_clears_its_dialog_area() {
        // Regression test: the purge dialog must not erase the session list above and below it.
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::load(
            StcliEngine::new(directory.path().join("stcli.sqlite3")),
            Config::default(),
            None,
        )
        .unwrap();
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let before = terminal.backend().buffer()[(20, 1)].clone();

        app.popup = Some(Popup::ConfirmDelete {
            session_id: stcli_core::EntityId::new(),
            name: "Test".to_owned(),
        });
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let after = &terminal.backend().buffer()[(20, 1)];

        assert_eq!(after.symbol(), before.symbol());
        assert_eq!(after.style(), before.style());
    }

    #[test]
    fn provider_list_popup_uses_half_width_and_seventy_percent_height() {
        // Regression test: the provider list must remain distinct from the underlying screen.
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::load(
            StcliEngine::new(directory.path().join("stcli.sqlite3")),
            Config::default(),
            None,
        )
        .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::empty()));
        let backend = TestBackend::new(100, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(25, 6)].symbol(), "┌");
        assert_eq!(buffer[(74, 6)].symbol(), "┐");
        assert_eq!(buffer[(25, 33)].symbol(), "└");
        assert_eq!(buffer[(74, 33)].symbol(), "┘");
    }

    #[test]
    fn new_modals_render_without_panicking_and_display_expected_titles() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        drop(store);
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        // 1. Test NewSession popup render
        app.open_new_session_popup();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(rendered.contains("Create New Session"));

        // 2. Test ImportArtifact popup render
        app.popup = Some(Popup::ImportArtifact(crate::app::ImportArtifactState {
            expected_kind: None,
            return_to: crate::app::ModalTarget::Sessions,
            input: String::new(),
        }));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(rendered.contains("Import Character Card from File"));

        // 3. Test ProviderProfile popup render
        app.open_provider_profile_popup(None, crate::app::ModalTarget::Sessions);
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(rendered.contains("New Provider Profile"));
    }
}
