use super::*;
use crate::appearance::Appearance;

pub(super) fn draw(frame: &mut Frame, app: &mut App, selected: Appearance) {
    let outer = frame.area().inner(Margin::new(2, 1));
    let offset = if outer.width >= 100 { 6 } else { 4 };
    let width = outer.width.min(DIALOG_WIDTH);
    let shortcuts = [("↑↓", "select"), ("↵", "apply"), ("esc", "cancel")];
    let hints = key_hints(&shortcuts).line_count(width.saturating_sub(4)) as u16;
    let rows = if app.appearance_error.is_some() { 8 } else { 3 };
    let area = Rect {
        y: outer.y + offset.min(outer.height),
        width,
        height: (rows + 3 + hints).min(outer.height.saturating_sub(offset)),
        ..outer
    };
    frame.render_widget(ratatui::widgets::Clear, area);
    app.controls.hits.clear();
    let (body, _) = draw_dialog(frame, area, "appearance", rows as usize, false, &shortcuts);
    let mut lines = Vec::new();
    for value in Appearance::ALL {
        let line = Line::from(format!(
            " {} {}",
            if value == app.engine.appearance {
                "●"
            } else {
                "○"
            },
            value.label()
        ));
        lines.push(if value == selected {
            line.style(dialog_selection())
        } else {
            line
        });
    }
    if let Some(error) = &app.appearance_error {
        lines.push(Line::default());
        lines.push(Line::from(error.as_str()).fg(RED));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
}
