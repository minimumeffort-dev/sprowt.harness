use super::*;
pub(super) fn draw(frame: &mut Frame, app: &mut App) {
    let editor = app.worker_settings_editor.as_ref().unwrap();
    let outer = frame.area().inner(Margin::new(2, 1));
    let area = Rect {
        width: outer.width.min(88),
        height: outer
            .height
            .min((editor.settings.executors.len() + 13) as u16),
        ..outer
    };
    frame.render_widget(ratatui::widgets::Clear, area);
    app.controls.hits.clear();
    let shortcuts = [
        ("↑↓", "row"),
        ("tab", "field"),
        ("←→", "change"),
        ("+/-", "row"),
        ("r", "refresh"),
        ("s", "save"),
        ("esc", "cancel"),
    ];
    let (body, _) = draw_dialog(
        frame,
        area,
        "workers",
        editor.settings.executors.len() + 8,
        false,
        &shortcuts,
    );
    let mut lines = vec![
        Line::from(format!(
            " {} simultaneous execution workers",
            editor.settings.capacity()
        ))
        .fg(MUTED),
        Line::default(),
    ];
    for row in 0..editor.settings.executors.len() + 2 {
        let choice = editor.choice(row);
        let label = if row == 0 {
            "Planner".into()
        } else if row == editor.settings.executors.len() + 1 {
            "Reviewer".into()
        } else {
            format!("Executor {}", row)
        };
        let count = if row > 0 && row <= editor.settings.executors.len() {
            format!("{}×", editor.settings.executors[row - 1].count)
        } else {
            String::new()
        };
        let values = [
            format!("{count:>3}"),
            format!("{} · {}", choice.provider.name(), choice.model),
            choice.effort.clone(),
        ];
        let mut spans = vec![Span::raw(format!(" {label:<12} "))];
        for (field, value) in values.into_iter().enumerate() {
            spans.push(
                if editor.row == row
                    && (editor.field == field
                        || editor.field == 0
                            && field == 1
                            && (row == 0 || row == editor.settings.executors.len() + 1))
                {
                    Span::styled(value, dialog_selection())
                } else {
                    Span::raw(value)
                },
            );
            spans.push(Span::raw("  "));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::default());
    lines.push(
        Line::from(" Quota fallback uses another selected model. New assignments only.").fg(MUTED),
    );
    if app.engine.models.catalog.loading {
        lines.push(Line::from(" Loading provider models…").fg(MUTED));
    }
    for error in app
        .engine
        .models
        .catalog
        .errors
        .iter()
        .chain(editor.error.iter())
    {
        lines.push(Line::from(error.as_str()).fg(RED));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((
            editor
                .row
                .saturating_add(3)
                .saturating_sub(body.height.saturating_sub(2) as usize) as u16,
            0,
        )),
        body,
    );
}
