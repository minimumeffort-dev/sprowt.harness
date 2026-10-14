mod evidence;
use super::*;
use crate::app::{
    controls::{Control, WorkspaceView},
    workspace_navigation::Focus,
};
use evidence::document;

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

pub(super) fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(m) = app.current_mod() else { return };
    app.workspace.reset_for((
        m.id,
        m.planning
            .as_ref()
            .map_or(String::new(), |p| p.source.clone()),
    ));
    let views = app.engine.workspace_views();
    if !views.contains(&app.workspace.inspector.tab) {
        app.workspace.selected = None;
        app.workspace.inspector.select(None);
        app.workspace.inspector.set_tab(WorkspaceView::Conversation);
    }
    let tab = app.workspace.inspector.tab;
    let count = app
        .current_mod()
        .map_or(0, |m| app.worker_summaries(m).len());
    let labels: Vec<_> = views
        .into_iter()
        .map(|view| {
            let shortcut = view as usize + 1;
            let label = if view == WorkspaceView::Agents && count > 0 {
                format!(" {} {} {count} ", shortcut, view.label())
            } else {
                format!(" {} {} ", shortcut, view.label())
            };
            (view, label)
        })
        .collect();
    let mut tab_rows = 1;
    let mut used = 0;
    for (_, label) in &labels {
        let width = label.len() as u16;
        if used > 0 && used + width > area.width {
            tab_rows += 1;
            used = 0;
        }
        used += width + 1;
    }
    let [tabs, hint, body] = Layout::vertical([
        Constraint::Length(tab_rows),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    let mut x = tabs.x;
    let mut y = tabs.y;
    for (view, label) in labels {
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
            Rect::new(x, y, width.min(tabs.width), 1),
            Line::from(label),
            Control::Tab(view),
            tab == view,
        );
        x += width + 1;
    }
    app.workspace.inspector.select(app.workspace.selected);
    if app.workspace.selected.is_some()
        && !app
            .workspace
            .selected
            .is_some_and(|key| app.valid_target(key))
    {
        app.workspace.selected = None;
        app.workspace.inspector.select(None);
    }
    if tab == WorkspaceView::Findings {
        let ids = app.review_finding_ids();
        if !app
            .workspace
            .inspector
            .finding
            .is_some_and(|id| ids.contains(&id))
        {
            app.workspace.inspector.finding = ids.first().copied();
        }
    }
    let document = document(app, app.workspace.selected, body.width.saturating_sub(2));
    app.workspace.agent_controls = document
        .controls
        .iter()
        .filter_map(|(row, control)| {
            if let Control::AgentDetail(detail) = control {
                Some((*detail, *row))
            } else {
                None
            }
        })
        .collect();
    if !app
        .workspace
        .agent_controls
        .iter()
        .any(|(detail, _)| app.workspace.agent_cursor == Some(*detail))
    {
        app.workspace.agent_cursor = None;
    }
    app.workspace.message_controls = document
        .controls
        .iter()
        .filter_map(|(row, control)| {
            if let Control::Message(message, segment) = control {
                Some((*message, *segment, *row))
            } else {
                None
            }
        })
        .collect();
    if !app
        .workspace
        .message_controls
        .iter()
        .any(|(message, segment, _)| app.workspace.message_cursor == Some((*message, *segment)))
    {
        app.workspace.message_cursor = None;
    }
    let hint_text = if app.workspace.focus == Focus::Composer {
        "tab Explore"
    } else if tab == WorkspaceView::Agents && !app.workspace.agent_controls.is_empty() {
        "tab Write · [ ] Details · ↵ Toggle"
    } else if tab == WorkspaceView::Conversation && !app.workspace.message_controls.is_empty() {
        if hint.width < 60 {
            "tab Write · [ ] Details · ↵ Toggle"
        } else {
            "tab Write · ↑↓ Scroll · [ ] Message details · ↵ Toggle"
        }
    } else {
        "tab Write · ↑↓ Scroll"
    };
    frame.render_widget(Line::from(hint_text).fg(KEY_HINT), hint);
    let (offset, maximum) = draw_text(
        frame,
        Paragraph::new(document.lines),
        body,
        app.workspace.inspector.offset,
        app.workspace.focus != Focus::Composer,
    );
    app.workspace.inspector.area = body;
    app.workspace.inspector.offset = offset;
    app.workspace.inspector.maximum = maximum;
    app.page_size = body.height.max(1);
    app.scroll.content = body;
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
