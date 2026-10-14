use super::*;
use crate::app::{
    controls::Control,
    projects::{PickerPanel, ProjectRow},
};

pub(super) fn draw(frame: &mut Frame, app: &mut App, index: usize, area: Rect) {
    if let PickerPanel::Open(input) = &app.project_picker {
        let (body, _) = draw_dialog(
            frame,
            area,
            "open project",
            5,
            false,
            &[("↵", "open"), ("esc", "back")],
        );
        frame.render_widget(
            input.as_ref(),
            Rect {
                height: 3.min(body.height),
                ..body
            },
        );
        if let Some(error) = &app.project_picker_error {
            frame.render_widget(
                Paragraph::new(error.as_str())
                    .fg(RED)
                    .wrap(Wrap { trim: false }),
                Rect {
                    y: body.y + 3,
                    height: body.height.saturating_sub(3),
                    ..body
                },
            );
        }
        return;
    }
    if let PickerPanel::ProjectMenu(id) = app.project_picker {
        let project = app.projects.iter().find(|p| p.id == id);
        let (body, _) = draw_dialog(
            frame,
            area,
            "project actions",
            4,
            false,
            &[
                ("↵", "open project"),
                ("tab", "active / closed"),
                ("x", "remove from sidebar"),
                ("esc", "back"),
            ],
        );
        let mut text = vec![
            Line::from(project.map_or("Project", |p| p.name.as_str())).fg(ACCENT),
            Line::from(project.map_or("", |p| p.path.as_str())).fg(MUTED),
        ];
        if let Some(error) = &app.project_picker_error {
            text.push(Line::from(error.as_str()).fg(RED));
        }
        frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), body);
        return;
    }
    let rows = app.project_rows();
    let mut hints = if rows.is_empty() {
        vec![("+", "project")]
    } else {
        vec![
            ("↑↓", "select"),
            ("↵", "open / fold"),
            ("n", "new"),
            (".", "menu"),
            ("+", "project"),
        ]
    };
    if matches!(rows.get(index), Some(ProjectRow::Codemod { .. })) {
        hints.extend(if app.show_closed {
            vec![("r", "reopen"), ("d", "delete")]
        } else {
            vec![("c", "close"), ("d", "delete")]
        });
    }
    hints.extend([
        ("tab", if app.show_closed { "active" } else { "closed" }),
        ("esc", "back"),
    ]);
    let (body, _) = draw_dialog(
        frame,
        area,
        if app.show_closed {
            "projects · closed codemods"
        } else {
            "projects"
        },
        rows.len().max(1) + usize::from(app.project_picker_error.is_some()) * 2,
        false,
        &hints,
    );
    app.scroll.content = body;
    let list_area = Rect {
        height: body
            .height
            .saturating_sub(if app.project_picker_error.is_some() {
                2
            } else {
                0
            }),
        ..body
    };
    let items = rows.iter().map(|row| {
        let project = app.projects.iter().find(|p| p.id == row.project()).unwrap();
        match row {
            ProjectRow::Project(_) => Line::from(vec![
                "▱ ".fg(ACCENT),
                Span::raw(fit_name(&project.name, list_area.width.saturating_sub(12))),
                if project.id == app.project_id {
                    Span::raw("")
                } else {
                    match project.activity {
                        crate::store::ProjectActivity::Idle => Span::raw(""),
                        crate::store::ProjectActivity::Working => " •".fg(ACCENT),
                        crate::store::ProjectActivity::Attention => " !".fg(RED),
                    }
                },
            ]),
            ProjectRow::Codemod { id, .. } => {
                let m = project.mods.iter().find(|m| m.id == *id).unwrap();
                Line::from(vec![
                    format!("   {} ", if m.closed { "□" } else { MOD_GLYPH }).fg(MUTED),
                    Span::raw(fit_name(&m.name, list_area.width.saturating_sub(7))),
                    (if app.current_mod().is_some_and(|m| m.id == *id)
                        && app.navigation.current() != crate::engine::Destination::NewCodemod
                    {
                        " ✓"
                    } else {
                        ""
                    })
                    .fg(ACCENT),
                ])
            }
            ProjectRow::Empty(_) => Line::from(if app.show_closed {
                "   No closed codemods"
            } else {
                "   No codemods"
            })
            .fg(MUTED),
        }
    });
    let mut state =
        ListState::default().with_selected(Some(index.min(rows.len().saturating_sub(1))));
    frame.render_stateful_widget(
        List::new(items).highlight_style(dialog_selection()),
        list_area,
        &mut state,
    );
    if rows.is_empty() {
        frame.render_widget(Paragraph::new("No projects").fg(MUTED), list_area);
    }
    for (row, entry) in (state.offset()..rows.len())
        .take(list_area.height as usize)
        .enumerate()
    {
        let line = Rect {
            y: list_area.y + row as u16,
            height: 1,
            ..list_area
        };
        app.controls.add(line, Control::PickMod(entry));
        if let ProjectRow::Project(id) = rows[entry] {
            let menu = Rect::new(line.right().saturating_sub(5), line.y, 2, 1);
            let new = Rect::new(line.right().saturating_sub(2), line.y, 2, 1);
            frame.render_widget(Line::from("⋯").fg(MUTED), menu);
            frame.render_widget(Line::from("✎").fg(ACCENT), new);
            app.controls.add(menu, Control::ProjectMenu(id));
            app.controls.add(new, Control::ProjectNew(id));
        }
    }
    if let Some(error) = &app.project_picker_error {
        frame.render_widget(
            Paragraph::new(error.as_str())
                .fg(RED)
                .wrap(Wrap { trim: false }),
            Rect::new(body.x, list_area.bottom(), body.width, 2),
        );
    }
}
