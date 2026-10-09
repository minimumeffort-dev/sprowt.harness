use super::*;
use crate::app::timeline::{Key, State};

pub(super) fn draw(frame: &mut Frame, app: &mut App, area: Rect, elapsed: Option<Duration>) {
    if area.is_empty() {
        return;
    }
    let m = app.current_mod().unwrap();
    app.timeline
        .reset_for((m.id, m.planning.as_ref().unwrap().source.clone()));
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
        .map_or(Key::Publish, |i| i.key);
    if app.timeline.selected.is_none() {
        app.timeline.selected = Some(current);
    }
    let width = area.width;
    let hints = if app.timeline.focused {
        key_hints(&[
            ("↑↓", "select"),
            ("↵", "details"),
            ("space", "expand"),
            ("tab", "message"),
        ])
    } else {
        Paragraph::new(Line::from(vec![
            "Timeline  ".fg(ACCENT).bold(),
            "tab Explore".fg(KEY_HINT),
        ]))
    };
    let hints_height = hints.line_count(width).min(2) as u16;
    let [heading, body] =
        Layout::vertical([Constraint::Length(hints_height.max(1)), Constraint::Min(0)]).areas(area);
    frame.render_widget(hints, heading);
    app.timeline.jump_area = Rect::default();
    if app.timeline.held {
        let label = if app.timeline.focused {
            "end Jump to current"
        } else {
            "tab end Jump to current"
        };
        let button = Rect {
            x: heading.right().saturating_sub(label.len() as u16),
            width: label.len() as u16,
            height: 1,
            ..heading
        };
        if button.x > heading.x + if app.timeline.focused { 58 } else { 23 } {
            frame.render_widget(Line::from(label).fg(ACCENT).bg(CONTROL), button);
            app.timeline.jump_area = button;
        } else {
            frame.render_widget(
                Line::from(if app.timeline.focused {
                    "end Current · tab Message"
                } else {
                    "Timeline · tab end Current"
                })
                .fg(ACCENT),
                heading,
            );
            app.timeline.jump_area = Rect {
                x: heading.x + if app.timeline.focused { 0 } else { 11 },
                width: if app.timeline.focused { 11 } else { 15 },
                height: 1,
                ..heading
            };
        }
    }
    app.scroll.content = body;
    app.page_size = body.height.max(1);
    let mut lines = Vec::new();
    let mut positions = Vec::new();
    let dock = app.action_dock();
    for item in &items {
        let top = lines.len();
        let selected = app.timeline.focused && app.timeline.selected == Some(item.key);
        let open = app
            .timeline
            .expanded
            .get(&item.key)
            .copied()
            .unwrap_or(item.expanded_by_default());
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
        if selected {
            title = title.bg(SELECTED);
        }
        if item.key == Key::Request {
            title = title.bg(USER_BACKGROUND);
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
            for text in &item.detail {
                if text.trim().is_empty() {
                    continue;
                }
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
        positions.push((item.key, top, lines.len()));
        lines.push(Line::from(if item.key == Key::Request { "" } else { "│" }).fg(BORDER));
    }
    lines.pop();
    let max = lines.len().saturating_sub(body.height as usize);
    let target = if app.timeline.focused {
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
        body,
    );
}
