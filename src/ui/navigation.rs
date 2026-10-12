use super::*;
use crate::app::controls::Control;

pub(super) fn draw(frame: &mut Frame, app: &mut App, area: Rect) -> Rect {
    let state = app.navigation_snapshot();
    let compact = area.width < 70;
    let mut items = Vec::new();
    if state.can_go_back {
        items.push((
            if compact {
                "‹ Alt←"
            } else {
                "‹ Back Alt←"
            },
            Control::NavigateBack,
        ));
    }
    if state.can_go_forward {
        items.push((
            if compact {
                "› Alt→"
            } else {
                "› Forward Alt→"
            },
            Control::NavigateForward,
        ));
    }
    let width = items
        .iter()
        .map(|(text, _)| Line::from(*text).width() as u16)
        .sum::<u16>()
        + items.len().saturating_sub(1) as u16 * 3;
    let mut x = area.right().saturating_sub(width);
    for (label, control) in items {
        let button = Rect::new(x, area.y, Line::from(label).width() as u16, 1);
        frame.render_widget(Line::from(label).fg(KEY_HINT), button);
        app.controls.add(button, control);
        x += button.width + 3;
    }
    Rect {
        width: area.width.saturating_sub(width + u16::from(width > 0) * 2),
        ..area
    }
}
