use super::*;
use crate::{app::worker_settings::Editor, worker_settings::catalog::Model};

pub(super) fn draw(frame: &mut Frame, app: &mut App) {
    let editor = app.worker_settings_editor.as_ref().unwrap();
    let outer = frame.area().inner(Margin::new(2, 1));
    let width = outer.width.min(88);
    let (mut lines, selected) = settings_lines(
        editor,
        &app.engine.models.catalog.models,
        width.saturating_sub(4),
    );
    if app.engine.models.catalog.loading {
        lines.push(Line::from("Loading provider models…").fg(MUTED));
    }
    for error in app
        .engine
        .models
        .catalog
        .errors
        .iter()
        .chain(editor.error.iter())
    {
        lines.extend(wrap_line(
            Line::from(error.clone()).fg(RED),
            width.saturating_sub(4),
            0,
        ));
    }
    let area = Rect {
        width,
        height: outer.height.min((lines.len() + 6) as u16),
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
    let (body, _) = draw_dialog(frame, area, "workers", lines.len(), false, &shortcuts);
    let scroll = selected.end.saturating_sub(body.height as usize);
    frame.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), body);
}

fn settings_lines(
    editor: &Editor,
    models: &[Model],
    width: u16,
) -> (Vec<Line<'static>>, std::ops::Range<usize>) {
    let mut lines = Vec::new();
    let mut selected = 0..0;
    for row in 0..editor.settings.executors.len() + 2 {
        if row == 1 {
            lines.push(Line::default());
            lines.extend(wrap_line(
                Line::from(format!(
                    "Execution team · {} maximum",
                    editor.settings.capacity()
                ))
                .fg(ACCENT)
                .bold(),
                width,
                0,
            ));
            lines.push(Line::default());
        } else if row == editor.settings.executors.len() + 1 {
            lines.push(Line::default());
        }
        let choice = editor.choice(row);
        let label = if row == 0 {
            "Planner"
        } else if row == editor.settings.executors.len() + 1 {
            "Reviewer"
        } else {
            ""
        };
        let count = if row > 0 && row <= editor.settings.executors.len() {
            format!("{} ×", editor.settings.executors[row - 1].count)
        } else {
            String::new()
        };
        let model = models
            .iter()
            .find(|model| model.provider == choice.provider && model.model == choice.model)
            .map_or(choice.model.as_str(), |model| model.label.as_str());
        let values = [
            if label.is_empty() {
                format!("{count:<13}")
            } else {
                "   ".into()
            },
            format!("{} · {}", choice.provider.name(), model),
            choice.effort.clone(),
        ];
        let mut spans = Vec::new();
        if !label.is_empty() {
            spans.push(Span::styled(format!("{label:<10}"), Style::new().fg(MUTED)));
        }
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
        let start = lines.len();
        lines.extend(wrap_line(Line::from(spans), width, 10));
        if editor.row == row {
            selected = start..lines.len();
        }
    }
    lines.push(Line::default());
    for text in [
        "Quota fallback: another selected model",
        "Changes apply to new assignments.",
    ] {
        lines.extend(wrap_line(Line::from(text).fg(MUTED), width, 0));
    }
    (lines, selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_team_keeps_selected_reviewer_visible_after_wrapping() {
        let editor = Editor {
            settings: crate::worker_settings::tests::team(),
            mod_id: None,
            row: 3,
            field: 1,
            error: None,
        };
        let catalog = crate::worker_settings::tests::catalog();
        let (lines, selected) = settings_lines(&editor, &catalog.models, 40);
        let text: Vec<_> = lines.iter().map(ToString::to_string).collect();
        assert_eq!(text[3], "Execution team · 3 maximum");
        assert_eq!(
            &text[selected.clone()],
            &["Reviewer       codex · gpt-6-astra ", "          high  "]
        );
        assert_eq!(text.last().unwrap(), "Changes apply to new assignments.");
        assert!(lines.iter().all(|line| line.width() <= 40));
        assert!(
            lines[selected]
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| {
                    span.content.contains("gpt-6-astra") && span.style.bg == dialog_selection().bg
                })
        );
    }
}
