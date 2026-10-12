use super::*;
use crate::app::{controls::Control, formatting::Format};

pub(super) fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let selected = app.formatting_menu.as_ref().unwrap().selected;
    let shortcuts = [("↑↓", "select"), ("↵", "apply"), ("esc", "cancel")];
    let width = area.width.min(40);
    let area = Rect {
        width,
        height: (Format::ALL.len() as u16
            + 3
            + key_hints(&shortcuts).line_count(width.saturating_sub(4)) as u16)
            .min(area.height),
        ..area
    };
    frame.render_widget(ratatui::widgets::Clear, area);
    let (body, _) = draw_dialog(
        frame,
        area,
        "format request",
        Format::ALL.len(),
        false,
        &shortcuts,
    );
    let items: Vec<_> = Format::ALL
        .into_iter()
        .map(|format| {
            ratatui::widgets::ListItem::new(Line::from(vec![
                format!(" {:>2}  ", format.glyph()).fg(ACCENT),
                format.label().fg(KEY_HINT),
            ]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        List::new(items).highlight_style(dialog_selection()),
        body,
        &mut state,
    );
    for (row, format) in Format::ALL.into_iter().skip(state.offset()).enumerate() {
        if row >= body.height as usize {
            break;
        }
        app.controls.add(
            Rect::new(body.x, body.y + row as u16, body.width, 1),
            Control::FormatItem(format),
        );
    }
}
