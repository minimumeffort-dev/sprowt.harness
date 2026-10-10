use super::*;
use crate::app::timeline::{Focus, Item, Key, State};

pub(super) fn draw(frame: &mut Frame, app: &mut App, area: Rect, elapsed: Option<Duration>) {
    if area.is_empty() {
        return;
    }
    let m = app.current_mod().unwrap();
    app.timeline
        .reset_for((m.id, m.planning.as_ref().unwrap().source.clone()));
    let split = area.width >= 120 && area.height >= 12;
    let (area, inspector) = if split {
        let [left, divider, right] = Layout::horizontal([
            Constraint::Percentage(45),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .areas(area);
        frame.render_widget(
            Block::default()
                .borders(ratatui::widgets::Borders::LEFT)
                .border_style(Style::new().fg(BORDER)),
            Rect {
                x: divider.x + 1,
                width: 1,
                ..divider
            },
        );
        (left, right)
    } else {
        if app.timeline.focus == Focus::Inspector {
            app.timeline.focus = Focus::Timeline;
            app.timeline.reveal = true;
        }
        (area, Rect::default())
    };
    let items = app.timeline_items();
    if app.focus_plan {
        app.timeline.top = 0;
        app.timeline.held = true;
        app.timeline.anchor = None;
        app.timeline.reveal = false;
        app.focus_plan = false;
    }
    let current = items
        .iter()
        .find(|i| i.action.is_some())
        .or_else(|| items.iter().find(|i| i.state == State::Attention))
        .or_else(|| items.iter().find(|i| i.state == State::Active && i.branch))
        .or_else(|| items.iter().find(|i| i.state == State::Active))
        .or_else(|| items.iter().find(|i| i.state == State::Waiting))
        .or_else(|| items.last())
        .map_or(Key::Request, |i| i.key);
    if app.timeline.selected.is_none() || !app.timeline.held {
        app.timeline.selected = Some(current);
    }
    let focused = app.timeline.focus == Focus::Timeline;
    let width = area.width.saturating_sub(2);
    let [heading, hints, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    frame.render_widget(
        Line::from(if focused { "▸ Timeline" } else { "Timeline" })
            .fg(if focused { ACCENT } else { KEY_HINT })
            .bold(),
        heading,
    );
    frame.render_widget(
        if focused {
            key_hints(&[("↑↓", "select"), ("↵", "details"), ("space", "expand")])
        } else {
            key_hints(&[(
                if app.timeline.focus == Focus::Inspector {
                    "←"
                } else {
                    "tab"
                },
                "Explore",
            )])
        },
        hints,
    );
    app.timeline.jump_area = Rect::default();
    if app.timeline.held {
        let label = if focused {
            "end Current"
        } else {
            "Jump to current"
        };
        let button = Rect {
            x: heading.right().saturating_sub(label.len() as u16),
            width: label.len() as u16,
            ..heading
        };
        if button.x > heading.x + 12 {
            frame.render_widget(Line::from(label).fg(ACCENT).bg(CONTROL), button);
            app.timeline.jump_area = button;
        }
    }
    app.scroll.content = body;
    app.page_size = body.height.max(1);
    let mut lines = Vec::new();
    let mut positions = Vec::new();
    let dock = app.action_dock();
    for item in &items {
        let top = lines.len();
        let selected = split && app.timeline.selected == Some(item.key)
            || focused && app.timeline.selected == Some(item.key);
        let open =
            app.timeline
                .expanded
                .get(&item.key)
                .copied()
                .unwrap_or(if item.key == Key::Build {
                    item.state != State::Done
                } else {
                    !split && item.expanded_by_default()
                });
        let (marker, color) = match item.state {
            State::Done => ("✓", ACCENT),
            State::Active => (activity_glyph(elapsed), ACCENT),
            State::Attention => ("!", Color::Red),
            State::Waiting => ("○", KEY_HINT),
            State::Previous => ("·", KEY_HINT),
        };
        let prefix = if item.key == Key::Request {
            "> ".to_owned()
        } else if item.branch {
            format!("├─ {marker} ")
        } else {
            format!("{marker} ")
        };
        let mut title = Line::from(vec![
            prefix.clone().fg(color),
            item.title.lines().next().unwrap_or("").to_owned().bold(),
        ]);
        if item.key != Key::Request && (item.key == Key::Build || !item.detail.is_empty()) {
            title.spans.push(
                format!(
                    " {}",
                    if open
                        || item.key == Key::Build
                            && app
                                .timeline
                                .expanded
                                .get(&Key::Build)
                                .copied()
                                .unwrap_or(item.state != State::Done)
                    {
                        "▾"
                    } else {
                        "▸"
                    }
                )
                .fg(KEY_HINT),
            );
        }
        if item.key == Key::Request {
            title = title.bg(USER_BACKGROUND);
        }
        if selected {
            title = title.bg(SELECTED);
        }
        let mut rows = wrap_line(title, width, if item.branch { 5 } else { 2 });
        if item.key == Key::Request {
            for line in item.title.lines().skip(1) {
                rows.extend(wrap_line(
                    Line::from(format!("  {line}")).bg(USER_BACKGROUND),
                    width,
                    2,
                ));
            }
        }
        if item.key == Key::Request && !open {
            if rows.len() > 2 {
                rows.truncate(2);
                rows[1] = Line::from(format!(
                    "{}…",
                    fit_name(&rows[1].to_string(), width.saturating_sub(1))
                ))
                .bg(USER_BACKGROUND);
            }
            lines.extend(rows);
        } else {
            lines.extend(rows);
        }
        if open {
            let prefix = if item.key == Key::Request {
                "  "
            } else if item.branch {
                "│    "
            } else {
                "│ "
            };
            // The side pane owns task evidence; expanded rows only identify the worker.
            let limit = if split && matches!(item.key, Key::Task(_) | Key::Repair(_)) {
                1
            } else {
                usize::MAX
            };
            for text in item.detail.iter().take(limit) {
                if text.trim().is_empty() {
                    continue;
                }
                if matches!(item.key, Key::Failure(_)) && !split {
                    for mut row in findings::saved_lines(text, width.saturating_sub(5)) {
                        row.spans.insert(0, prefix.fg(BORDER));
                        lines.push(row);
                    }
                    continue;
                }
                let summary;
                let text = if matches!(item.key, Key::Failure(_)) {
                    summary = findings::saved_summary(text);
                    &summary
                } else {
                    text
                };
                let text = if item.key == Key::Request {
                    text.as_str()
                } else {
                    text.lines().next().unwrap_or(text)
                };
                let mut detail = wrap_line(
                    Line::from(text.to_owned()).fg(KEY_HINT),
                    width.saturating_sub(prefix.chars().count() as u16),
                    0,
                );
                if detail.len() > 3 && item.key != Key::Request {
                    detail.truncate(3);
                    detail[2] = Line::from(fit_name(
                        &format!("{}…", detail[2]),
                        width.saturating_sub(prefix.chars().count() as u16),
                    ))
                    .fg(KEY_HINT);
                }
                for mut row in detail {
                    row.spans.insert(0, prefix.fg(BORDER));
                    lines.push(row);
                }
            }
        }
        if let Some(action) = item
            .action
            .and_then(|action| dock.actions.iter().find(|a| a.action == action))
        {
            let mut row = action_control(action, width.saturating_sub(2), false);
            row.spans.insert(0, "│ ".fg(BORDER));
            lines.extend(wrap_line(row, width, 2));
        }
        lines.push(Line::from(if item.key == Key::Request { "" } else { "│" }).fg(BORDER));
        // The separator belongs to this row's scroll anchor too.
        positions.push((item.key, top, lines.len()));
    }
    lines.pop();
    if let Some((_, _, end)) = positions.last_mut() {
        *end = lines.len();
    }
    let max = lines.len().saturating_sub(body.height as usize);
    let target = if app.timeline.focus != Focus::Composer {
        app.timeline.selected.unwrap_or(current)
    } else {
        current
    };
    let target_top = positions
        .iter()
        .find(|(key, _, _)| *key == target)
        .map_or(0, |(_, start, _)| *start);
    if app.timeline.reveal || !app.timeline.held {
        if app.timeline.reveal
            || target_top < app.timeline.top
            || target_top >= app.timeline.top + body.height as usize
        {
            app.timeline.top = target_top.saturating_sub(2).min(max);
        }
    } else if let Some((key, offset)) = app.timeline.anchor
        && let Some((_, start, end)) = positions.iter().find(|(k, _, _)| *k == key)
    {
        app.timeline.top = (start + offset.min(end - start - 1)).min(max);
    }
    app.timeline.top = app.timeline.top.min(max);
    app.timeline.reveal = false;
    app.timeline.rows = positions;
    app.timeline.anchor = app
        .timeline
        .rows
        .iter()
        .rev()
        .find(|(_, start, _)| *start <= app.timeline.top)
        .map(|(key, start, _)| (*key, app.timeline.top - start));
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((app.timeline.top.min(u16::MAX as usize) as u16, 0)),
        Rect { width, ..body },
    );
    draw_scrollbar(
        frame,
        body,
        app.timeline.rows.last().map_or(0, |(_, _, end)| *end),
        app.timeline.top,
        focused,
    );
    if split {
        draw_inspector(frame, app, &items, inspector);
    }
}

fn draw_inspector(frame: &mut Frame, app: &mut App, items: &[Item], area: Rect) {
    let focused = app.timeline.focus == Focus::Inspector;
    let [heading, hints, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    frame.render_widget(
        Line::from(if focused { "▸ Details" } else { "Details" })
            .fg(if focused { ACCENT } else { KEY_HINT })
            .bold(),
        heading,
    );
    let selected = items.iter().find(|i| Some(i.key) == app.timeline.selected);
    let mut keys = Vec::new();
    if focused {
        keys.push(("↑↓", "scroll"));
        if selected.is_some_and(|i| i.inspect.is_some() || i.key == Key::Plan) {
            keys.push(("↵", "full view"));
        }
        if selected.is_some_and(|i| matches!(i.key, Key::Task(_) | Key::Repair(_) | Key::Checks)) {
            keys.push((
                "e",
                if app.show_evidence {
                    "hide evidence"
                } else {
                    "evidence"
                },
            ));
        }
        keys.push(("tab", "message"));
    } else if app.timeline.focus == Focus::Timeline {
        keys.push(("tab", "Focus details"));
    }
    frame.render_widget(key_hints(&keys), hints);
    let lines = selected.map_or_else(
        || vec![Line::from("This step is no longer in the current plan.").fg(KEY_HINT)],
        |item| inspector_lines(app, item, body.width.saturating_sub(2)),
    );
    if app.timeline.inspector.key != app.timeline.selected {
        app.timeline.inspector.offset = 0;
        app.timeline.inspector.key = app.timeline.selected;
    }
    let (offset, maximum) = draw_text(
        frame,
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        body,
        app.timeline.inspector.offset,
        focused,
    );
    app.timeline.inspector.area = body;
    app.timeline.inspector.offset = offset;
    app.timeline.inspector.maximum = maximum;
}

fn inspector_lines(app: &App, item: &Item, width: u16) -> Vec<Line<'static>> {
    let mut lines = match item.key {
        Key::Task(id) | Key::Repair(id) => tasks::task_lines(app, id, width),
        Key::Checks => inspector::check_summary(app, width),
        Key::Review => findings::finding_lines(app, width),
        Key::Failure(_) => {
            let mut lines = wrap_line(Line::from(item.title.clone()).bold(), width, 0);
            lines.push(Line::default());
            for detail in &item.detail {
                lines.extend(findings::saved_lines(detail, width));
            }
            lines
        }
        _ => {
            let mut lines = vec![Line::from(item.title.clone()).bold(), Line::default()];
            for detail in &item.detail {
                lines.extend(Text::from(detail.clone()).lines);
            }
            lines
        }
    };
    if let Some(plan) = app
        .current_mod()
        .and_then(|m| m.planning.as_ref())
        .and_then(|p| p.plan.as_ref())
    {
        match item.key {
            Key::Plan => {
                for (label, notes) in [
                    ("Contracts", &plan.contracts),
                    ("Assumptions", &plan.assumptions),
                ] {
                    if !notes.is_empty() {
                        lines.push(Line::default());
                        lines.push(Line::from(label).fg(KEY_HINT).bold());
                        lines.extend(notes.iter().map(|note| Line::from(format!("· {note}"))));
                    }
                }
            }
            Key::Build => {
                for (i, task) in plan.tasks.iter().enumerate() {
                    let run = app
                        .current_mod()
                        .and_then(|m| m.execution.as_ref())
                        .and_then(|e| e.tasks.iter().find(|r| r.task_id == task.id))
                        .and_then(|r| app.inspect_task(r.id));
                    lines.push(
                        Line::from(format!(
                            "{}. {} · {}",
                            i + 1,
                            task.title,
                            run.as_ref().map_or("Waiting", |r| r.state)
                        ))
                        .bold(),
                    );
                    lines.push(Line::from(task.outcome.clone()).fg(KEY_HINT));
                    lines.push(Line::default());
                }
            }
            Key::Planned(index) => {
                if let Some(task) = plan.tasks.get(index) {
                    lines.push(Line::default());
                    lines.push(
                        Line::from(format!("Files · {}", task.files.join(", "))).fg(KEY_HINT),
                    );
                    lines.push(Line::from("Checks").fg(KEY_HINT).bold());
                    lines.extend(
                        task.checks
                            .iter()
                            .map(|check| Line::from(format!("· {check}"))),
                    );
                }
            }
            _ => {}
        }
    }
    lines
}
