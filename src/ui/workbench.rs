mod evidence;
use super::*;
use crate::app::{
    controls::{Control, EvidenceTab},
    timeline::{Focus, Item, Key, State},
};
use evidence::{Document, document};

pub(super) fn button(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    label: Line<'static>,
    control: Control,
    selected: bool,
) {
    frame.render_widget(
        Block::new().bg(if selected { SELECTED } else { CONTROL }),
        area,
    );
    frame.render_widget(label.fg(if selected { ACCENT } else { KEY_HINT }), area);
    app.controls.add(area, control);
}

fn name(key: Key) -> &'static str {
    match key {
        Key::Request => "Request",
        Key::Plan => "Plan",
        Key::Environment => "Environment",
        Key::Build => "Implementation",
        Key::Checks => "Combined checks",
        Key::Review => "Review",
        Key::Publish => "Pull request",
        Key::Git => "Git activity",
        Key::Failure(_) => "Saved failure",
        _ => "Task",
    }
}

fn marker(state: State, elapsed: Option<Duration>) -> (&'static str, Color) {
    match state {
        State::Done => ("✓", ACCENT),
        State::Active => (activity_glyph(elapsed), ACCENT),
        State::Attention => ("!", Color::Red),
        State::Waiting => ("○", KEY_HINT),
        State::Previous => ("·", KEY_HINT),
    }
}

pub(super) fn draw(frame: &mut Frame, app: &mut App, area: Rect, elapsed: Option<Duration>) {
    let Some(m) = app.current_mod() else { return };
    app.timeline
        .reset_for((m.id, m.planning.as_ref().unwrap().source.clone()));
    if app.timeline.overview {
        let [header, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
        timeline::draw(frame, app, body, elapsed);
        let label = " v Workbench ";
        let target = Rect {
            x: header.right().saturating_sub(label.len() as u16),
            width: label.len() as u16,
            ..header
        };
        button(
            frame,
            app,
            target,
            Line::from(label),
            Control::Overview,
            false,
        );
        return;
    }
    let items = app.navigation_items();
    let current = items
        .iter()
        .find(|i| i.action.is_some())
        .or_else(|| items.iter().find(|i| i.state == State::Attention))
        .or_else(|| items.iter().find(|i| i.state == State::Active))
        .or_else(|| items.iter().find(|i| i.state == State::Waiting))
        .or_else(|| items.last())
        .map_or(Key::Request, |i| i.key);
    if !app.timeline.held || app.timeline.selected.is_none() {
        app.timeline.selected = Some(current);
    }
    let split = area.width >= 100 && area.height >= 12;
    let (rail, evidence) = if split {
        let [rail, divider, evidence] = Layout::horizontal([
            Constraint::Length((area.width * 28 / 100).clamp(25, 43)),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .areas(area);
        frame.render_widget(
            Block::new()
                .borders(ratatui::widgets::Borders::LEFT)
                .border_style(Style::new().fg(BORDER)),
            Rect {
                x: divider.x + 1,
                width: 1,
                ..divider
            },
        );
        (rail, evidence)
    } else if app.timeline.focus == Focus::Inspector {
        (Rect::default(), area)
    } else {
        (area, Rect::default())
    };
    if !rail.is_empty() {
        draw_rail(frame, app, &items, rail, elapsed);
    } else {
        app.scroll.content = Rect::default();
    }
    if !evidence.is_empty() {
        draw_evidence(frame, app, &items, evidence, !split);
    }
}

fn draw_rail(
    frame: &mut Frame,
    app: &mut App,
    items: &[Item],
    area: Rect,
    elapsed: Option<Duration>,
) {
    let [heading, hints, body, status, jump] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    frame.render_widget(
        Line::from(if app.timeline.focus == Focus::Timeline {
            "▸ Timeline"
        } else {
            "Timeline"
        })
        .fg(ACCENT)
        .bold(),
        heading,
    );
    frame.render_widget(
        Line::from(if app.timeline.focus == Focus::Timeline {
            "↑↓ Select  ↵ Evidence"
        } else {
            "tab Explore"
        })
        .fg(KEY_HINT),
        hints,
    );
    let mut lines = Vec::new();
    let mut rows = Vec::new();
    for item in items {
        let top = lines.len();
        let (glyph, color) = marker(item.state, elapsed);
        let selected = app.timeline.selected == Some(item.key);
        let label = if item.branch {
            item.title.clone()
        } else {
            name(item.key).to_owned()
        };
        let mut line = Line::from(vec![
            format!("{}{} ", if item.branch { "  " } else { " " }, glyph).fg(color),
            fit_name(
                &label,
                area.width.saturating_sub(if item.branch { 5 } else { 3 }),
            )
            .bold(),
        ])
        .bg(if selected { SELECTED } else { Color::Reset });
        if selected {
            line = line.fg(ACCENT);
        }
        lines.push(line);
        lines.push(Line::default());
        rows.push((item.key, top, lines.len()));
    }
    let max = lines.len().saturating_sub(body.height as usize);
    if app.timeline.reveal || !app.timeline.held {
        if let Some((_, start, _)) = rows
            .iter()
            .find(|(k, _, _)| Some(*k) == app.timeline.selected)
            && (*start < app.timeline.top || *start >= app.timeline.top + body.height as usize)
        {
            app.timeline.top = start.saturating_sub(2).min(max);
        }
    } else if let Some((key, offset)) = app.timeline.anchor
        && let Some((_, start, end)) = rows.iter().find(|(k, _, _)| *k == key)
    {
        app.timeline.top = (start + offset.min(end - start - 1)).min(max);
    }
    app.timeline.top = app.timeline.top.min(max);
    app.timeline.reveal = false;
    app.timeline.anchor = rows
        .iter()
        .rev()
        .find(|(_, start, _)| *start <= app.timeline.top)
        .map(|(key, start, _)| (*key, app.timeline.top - start));
    app.timeline.rows = rows;
    app.scroll.content = body;
    app.page_size = body.height.max(1);
    frame.render_widget(
        Paragraph::new(lines).scroll((app.timeline.top.min(u16::MAX as usize) as u16, 0)),
        body,
    );
    for (key, start, _) in &app.timeline.rows {
        if *start >= app.timeline.top && *start < app.timeline.top + body.height as usize {
            app.controls.add(
                Rect {
                    y: body.y + (*start - app.timeline.top) as u16,
                    height: 1,
                    ..body
                },
                Control::Stage(*key),
            );
        }
    }
    draw_scrollbar(
        frame,
        body,
        app.timeline.rows.last().map_or(0, |(_, _, end)| *end),
        app.timeline.top,
        app.timeline.focus == Focus::Timeline,
    );
    frame.render_widget(
        Line::from(if app.timeline.held {
            "Exploring saved step"
        } else {
            "Following current"
        })
        .fg(KEY_HINT),
        status,
    );
    app.timeline.jump_area = Rect::default();
    if app.timeline.held {
        let label = " ctrl+l Current ";
        let target = Rect {
            width: jump.width.min(label.len() as u16),
            ..jump
        };
        button(
            frame,
            app,
            target,
            Line::from(label),
            Control::Current,
            false,
        );
        app.timeline.jump_area = target;
    }
}

fn draw_evidence(frame: &mut Frame, app: &mut App, items: &[Item], area: Rect, narrow: bool) {
    app.timeline.inspector.select(app.timeline.selected);
    if app.timeline.selected == Some(Key::Review) {
        let ids = app.review_finding_ids();
        if !app
            .timeline
            .inspector
            .finding
            .is_some_and(|id| ids.contains(&id))
        {
            app.timeline.inspector.finding = ids.first().copied();
        }
    }
    let [heading, tabs, divider, body] = Layout::vertical([
        Constraint::Length(if area.height < 10 { 1 } else { 2 }),
        Constraint::Length(if area.width < 49 { 2 } else { 1 }),
        Constraint::Length(if area.height < 10 { 0 } else { 2 }),
        Constraint::Min(0),
    ])
    .areas(area);
    let selected = items.iter().find(|i| Some(i.key) == app.timeline.selected);
    let label = if narrow {
        " ← Stages "
    } else {
        " v Timeline overview "
    };
    let focused = app.timeline.focus == Focus::Inspector;
    frame.render_widget(
        Line::from(fit_name(
            &format!(
                "{}{} / evidence",
                if focused { "▸ " } else { "" },
                selected.map_or("Step", |i| name(i.key))
            ),
            heading.width.saturating_sub(label.len() as u16 + 1),
        ))
        .fg(if focused { ACCENT } else { KEY_HINT })
        .bold(),
        heading,
    );
    let target = Rect {
        x: heading.right().saturating_sub(label.len() as u16),
        width: label.len() as u16,
        height: 1,
        ..heading
    };
    button(
        frame,
        app,
        target,
        Line::from(label),
        if narrow {
            Control::Back
        } else {
            Control::Overview
        },
        false,
    );
    let mut x = tabs.x;
    let mut y = tabs.y;
    for (i, tab) in EvidenceTab::ALL.into_iter().enumerate() {
        let label = format!(" {} {} ", i + 1, tab.label());
        let width = label.len() as u16;
        if x + width > tabs.right() {
            x = tabs.x;
            y += 1;
        }
        if y >= tabs.bottom() {
            break;
        }
        button(
            frame,
            app,
            Rect {
                x,
                y,
                width,
                height: 1,
            },
            Line::from(label),
            Control::Tab(tab),
            app.timeline.inspector.tab == tab,
        );
        x += width + 1;
    }
    frame.render_widget(
        Line::from("─".repeat(divider.width as usize)).fg(BORDER),
        divider,
    );
    let document = selected.map_or_else(
        || Document::message("This step is no longer in the current plan.", body.width),
        |item| document(app, item, body.width.saturating_sub(2)),
    );
    let (offset, maximum) = draw_text(
        frame,
        Paragraph::new(document.lines),
        body,
        app.timeline.inspector.offset,
        app.timeline.focus == Focus::Inspector,
    );
    app.timeline.inspector.area = body;
    app.timeline.inspector.offset = offset;
    app.timeline.inspector.maximum = maximum;
    for (row, control) in document.controls {
        if row >= offset as usize && row < offset as usize + body.height as usize {
            app.controls.add(
                Rect {
                    y: body.y + (row - offset as usize) as u16,
                    height: 1,
                    width: body.width.saturating_sub(2),
                    ..body
                },
                control,
            );
        }
    }
}
