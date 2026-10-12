use super::*;
use crate::app::{controls::Control, formatting::Format};
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

fn editor() -> (TestData, App) {
    let data = TestData::new();
    let project = data.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let app = App::load(project, false, data.store()).unwrap();
    (data, app)
}

fn format(app: &mut App, format: Format) {
    key(app, KeyCode::F(2), KeyModifiers::NONE);
    for action in Format::ALL {
        if action == format {
            break;
        }
        key(app, KeyCode::Down, KeyModifiers::NONE);
    }
    key(app, KeyCode::Enter, KeyModifiers::NONE);
}

#[test]
fn formatting_menu_cancel_preserves_selection_cursor_and_literal_slash() {
    let (_data, mut app) = editor();
    paste(&mut app, "Keep café and 漢字");
    key(&mut app, KeyCode::Home, KeyModifiers::SHIFT);
    let cursor = app.input.cursor();
    let selection = app.input.selection_range();
    key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.input.lines(), ["Keep café and 漢字"]);
    assert_eq!(app.input.cursor(), cursor);
    assert_eq!(app.input.selection_range(), selection);
    assert!(app.formatting_menu.is_none() && app.engine.view == View::NewMod && !app.quit);

    let (_data, mut app) = editor();
    paste(&mut app, "  ");
    key(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.input.lines(), ["  /"]);
    assert_eq!(app.input.cursor(), (0, 3));
    assert!(app.engine.mods.is_empty() && !app.quit);
}

#[test]
fn formatting_slash_removes_only_its_trigger_and_continued_paths_stay_literal() {
    let (_data, mut app) = editor();
    paste(&mut app, "Keep /Users/horia/project\n  ");
    key(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.input.lines(), ["Keep /Users/horia/project", "  ****"]);
    assert_eq!(app.input.cursor(), (1, 4));
    assert!(app.engine.mods.is_empty());

    let (_data, mut app) = editor();
    key(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('U'), KeyModifiers::SHIFT);
    paste(&mut app, "sers/horia/project");
    assert_eq!(app.input.lines(), ["/Users/horia/project"]);
    assert!(app.formatting_menu.is_none());

    let (_data, mut app) = editor();
    key(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(app.input.lines(), ["/ax"]);
    assert!(app.formatting_menu.is_none());
}

#[test]
fn formatting_slash_does_not_intercept_paths_in_code_fences_or_existing_text() {
    for prefix in ["```sh\n", "~~~~\n", "````\n```\n", "Use "] {
        let (_data, mut app) = editor();
        paste(&mut app, prefix);
        key(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(app.formatting_menu.is_none(), "{prefix}");
        paste(&mut app, "Users/horia/project");
        assert_eq!(
            app.input.lines().join("\n"),
            format!("{prefix}/Users/horia/project")
        );
    }
    let (_data, mut app) = editor();
    paste(&mut app, "~~~sh\n/Users/horia/project\n~~~\n");
    key(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(app.formatting_menu.is_some());
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        app.input.lines(),
        ["~~~sh", "/Users/horia/project", "~~~", "# "]
    );
}

#[test]
fn formatting_actions_preserve_unicode_selection_and_full_undo_history() {
    for (action, expected, selection) in [
        (Format::Heading, "# café 漢字", ((0, 2), (0, 9))),
        (Format::Bold, "**café 漢字**", ((0, 2), (0, 9))),
        (Format::Italic, "*café 漢字*", ((0, 1), (0, 8))),
        (Format::BulletedList, "- café 漢字", ((0, 2), (0, 9))),
        (Format::Checklist, "- [ ] café 漢字", ((0, 6), (0, 13))),
        (Format::InlineCode, "`café 漢字`", ((0, 1), (0, 8))),
        (Format::CodeBlock, "```\ncafé 漢字\n```", ((1, 0), (1, 7))),
    ] {
        let (_data, mut app) = editor();
        paste(&mut app, "café 漢字");
        key(&mut app, KeyCode::Home, KeyModifiers::SHIFT);
        format(&mut app, action);
        assert_eq!(app.input.lines().join("\n"), expected, "{action:?}");
        assert_eq!(app.input.selection_range(), Some(selection), "{action:?}");
        assert!(app.engine.mods.is_empty() && app.engine.view == View::NewMod);
        let steps = if matches!(
            action,
            Format::Heading | Format::BulletedList | Format::Checklist
        ) {
            1
        } else {
            2
        };
        for _ in 0..steps {
            key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        }
        assert_eq!(app.input.lines(), ["café 漢字"], "{action:?}");
        for _ in 0..steps {
            key(
                &mut app,
                KeyCode::Char('z'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
        }
        assert_eq!(app.input.lines().join("\n"), expected, "{action:?}");
    }
}

#[test]
fn formatting_empty_request_inserts_syntax_and_keeps_the_caret_ready_to_type() {
    for (action, expected, cursor) in [
        (Format::Heading, "# ", (0, 2)),
        (Format::Bold, "****", (0, 2)),
        (Format::Italic, "**", (0, 1)),
        (Format::BulletedList, "- ", (0, 2)),
        (Format::Checklist, "- [ ] ", (0, 6)),
        (Format::InlineCode, "``", (0, 1)),
        (Format::CodeBlock, "```\n\n```", (1, 0)),
    ] {
        let (_data, mut app) = editor();
        format(&mut app, action);
        assert_eq!(app.input.lines().join("\n"), expected);
        assert_eq!(app.input.cursor(), cursor);
        assert!(app.input.selection_range().is_none());
        assert!(app.engine.mods.is_empty() && app.engine.view == View::NewMod);
        paste(&mut app, "café");
        let expected = match action {
            Format::Heading => "# café",
            Format::Bold => "**café**",
            Format::Italic => "*café*",
            Format::BulletedList => "- café",
            Format::Checklist => "- [ ] café",
            Format::InlineCode => "`café`",
            Format::CodeBlock => "```\ncafé\n```",
        };
        assert_eq!(app.input.lines().join("\n"), expected);
        assert!(app.engine.mods.is_empty());
    }
}

#[test]
fn formatting_emphasis_keeps_whitespace_outside_delimiters_and_undo_preserves_it() {
    for (action, source, expected, steps) in [
        (Format::Bold, "hello ", "**hello** ", 2),
        (Format::Bold, " hello", " **hello**", 2),
        (Format::Italic, "\tcafé 漢字 \t", "\t*café 漢字* \t", 2),
        (Format::Bold, " \t\u{a0}", " \t\u{a0}", 0),
        (Format::Italic, " \t\n  \n", " \t\n  \n", 0),
        (
            Format::Bold,
            " first \nsecond ",
            " **first** \n**second** ",
            4,
        ),
        (
            Format::Italic,
            " first \n \t\nsecond ",
            " *first* \n \t\n*second* ",
            4,
        ),
        (
            Format::Bold,
            "\n café \n\n 漢字 \n",
            "\n **café** \n\n **漢字** \n",
            4,
        ),
    ] {
        let (_data, mut app) = editor();
        paste(&mut app, source);
        key(&mut app, KeyCode::Home, KeyModifiers::NONE);
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        key(
            &mut app,
            KeyCode::Char('<'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        );
        key(&mut app, KeyCode::Home, KeyModifiers::SHIFT);
        let cursor = app.input.cursor();
        let selection = app.input.selection_range();
        format(&mut app, action);
        assert_eq!(
            app.input.lines().join("\n"),
            expected,
            "{action:?}: {source:?}"
        );
        assert!(app.engine.mods.is_empty() && app.engine.view == View::NewMod);
        if steps == 0 {
            assert_eq!(app.input.cursor(), cursor);
            assert_eq!(app.input.selection_range(), selection);
        }
        for _ in 0..steps {
            key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        }
        assert_eq!(app.input.lines().join("\n"), source);
        if steps == 0 {
            key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
            assert_eq!(app.input.lines(), [""]);
        }
    }
}

#[test]
fn formatting_emphasis_keeps_partial_selection_scope_and_each_line_boundary() {
    let (_data, mut app) = editor();
    paste(&mut app, "before  café 漢字 \t after");
    key(&mut app, KeyCode::Home, KeyModifiers::NONE);
    for _ in 0..7 {
        key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    }
    for _ in 0..10 {
        key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
    }
    assert_eq!(app.input.selection_range(), Some(((0, 7), (0, 17))));
    format(&mut app, Format::Bold);
    assert_eq!(app.input.lines(), ["before  **café 漢字** \t after"]);
    assert_eq!(app.input.selection_range(), Some(((0, 7), (0, 21))));
    assert_eq!(app.input.cursor(), (0, 21));
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(app.input.lines(), ["before  café 漢字 \t after"]);

    let (_data, mut app) = editor();
    paste(&mut app, "\t café \t\n  second  ");
    key(
        &mut app,
        KeyCode::Char('<'),
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    );
    key(&mut app, KeyCode::Home, KeyModifiers::SHIFT);
    format(&mut app, Format::Bold);
    assert_eq!(app.input.lines(), ["\t **café** \t", "  **second**  "]);
    assert_eq!(app.input.selection_range(), Some(((0, 0), (1, 14))));
    assert_eq!(app.input.cursor(), (1, 14));
    for _ in 0..4 {
        key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    }
    assert_eq!(app.input.lines(), ["\t café \t", "  second  "]);
}

#[test]
fn formatting_large_unicode_selection_avoids_cursor_coordinate_truncation() {
    let (_data, mut app) = editor();
    let source = "é".repeat(70_000);
    paste(&mut app, &source);
    key(&mut app, KeyCode::Home, KeyModifiers::SHIFT);
    format(&mut app, Format::Bold);
    assert_eq!(app.input.lines(), [format!("**{source}**")]);
    assert_eq!(app.input.selection_range(), Some(((0, 2), (0, 70_002))));
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(app.input.lines(), [source]);
}

#[test]
fn formatting_multiline_blocks_exclude_the_next_unselected_line() {
    for (action, expected) in [
        (Format::Heading, "# first\n# second\nthird"),
        (Format::Checklist, "- [ ] first\n- [ ] second\nthird"),
        (Format::CodeBlock, "```\nfirst\nsecond\n```\nthird"),
    ] {
        let (_data, mut app) = editor();
        paste(&mut app, "first\nsecond\nthird");
        key(&mut app, KeyCode::Home, KeyModifiers::NONE);
        key(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
        key(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(app.input.selection_range(), Some(((0, 0), (2, 0))));
        format(&mut app, action);
        assert_eq!(app.input.lines().join("\n"), expected);
        key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(app.input.lines(), ["first", "second", "third"]);
    }
}

#[test]
fn formatting_code_delimiters_preserve_embedded_fences_and_edge_backticks() {
    for (action, source, expected) in [
        (Format::InlineCode, "`flag`", "`` `flag` ``"),
        (Format::InlineCode, "a``b", "```a``b```"),
        (Format::InlineCode, " padded ", "`  padded  `"),
        (Format::CodeBlock, "```", "````\n```\n````"),
    ] {
        let (_data, mut app) = editor();
        paste(&mut app, source);
        key(&mut app, KeyCode::Home, KeyModifiers::SHIFT);
        format(&mut app, action);
        assert_eq!(app.input.lines().join("\n"), expected);
        key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(app.input.lines(), [source]);
    }
}

#[test]
fn formatting_paste_newline_and_create_use_the_full_request_without_implicit_submit() {
    let (_data, mut app) = editor();
    commit_project(&app.engine.project);
    key(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    paste(&mut app, "Users/horia\r\nKeep café\rAnd 漢字");
    assert_eq!(app.input.lines(), ["/Users/horia", "Keep café", "And 漢字"]);
    assert!(app.formatting_menu.is_none() && app.engine.mods.is_empty());
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(app.input.lines(), ["/"]);
    key(
        &mut app,
        KeyCode::Char('z'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
    paste(&mut app, "- [ ] Keep paths literal");
    assert_eq!(
        app.input.lines(),
        [
            "/Users/horia",
            "Keep café",
            "And 漢字",
            "- [ ] Keep paths literal"
        ]
    );
    assert!(app.engine.mods.is_empty());
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.engine.mods.len(), 1);
    assert_eq!(
        app.engine.mods[0].description,
        "/Users/horia\nKeep café\nAnd 漢字\n- [ ] Keep paths literal"
    );
    assert_eq!(
        app.engine.mods[0].messages[0].body,
        app.engine.mods[0].description
    );
    assert_eq!(app.input.lines(), [""]);
}

#[test]
fn formatting_menu_click_targets_and_narrow_layout_keep_creation_separate() {
    for width in [40, 80, 120] {
        let (_data, mut app) = editor();
        paste(&mut app, "Keep this request");
        let text = rows(&screen(&mut app, width, 28)).join("\n");
        assert!(
            text.contains("F2 Format") && text.contains("Create codemod"),
            "{text}"
        );
        let (area, _) = app
            .controls
            .hits
            .iter()
            .find(|(_, control)| *control == Control::Format)
            .unwrap();
        let position = (area.x, area.y);
        app.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: position.0,
            row: position.1,
            modifiers: KeyModifiers::NONE,
        }))
        .unwrap();
        let text = rows(&screen(&mut app, width, 28)).join("\n");
        assert!(
            text.contains("format request")
                && text.contains("Bulleted list")
                && text.contains("Code block"),
            "{text}"
        );
        assert!(
            !app.controls
                .hits
                .iter()
                .any(|(_, control)| *control == Control::Submit)
        );
        let (area, _) = app
            .controls
            .hits
            .iter()
            .find(|(_, control)| *control == Control::FormatItem(Format::Checklist))
            .unwrap();
        let position = (area.x, area.y);
        app.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: position.0,
            row: position.1,
            modifiers: KeyModifiers::NONE,
        }))
        .unwrap();
        assert_eq!(app.input.lines(), ["- [ ] Keep this request"]);
        assert!(app.formatting_menu.is_none() && app.engine.mods.is_empty());
    }
}

#[test]
fn formatting_menu_exact_render_keeps_glyphs_labels_and_hints_aligned() {
    for width in [40, 80] {
        let (_data, mut app) = editor();
        key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
        let buffer = screen(&mut app, width, 28);
        let (body, _) = app
            .controls
            .hits
            .iter()
            .find(|(_, control)| *control == Control::FormatItem(Format::Heading))
            .unwrap();
        let captured: Vec<_> = rows(&buffer)
            .into_iter()
            .skip(body.y as usize - 1)
            .take(11)
            .map(|row| {
                row.chars()
                    .skip(body.x as usize - 2)
                    .take(body.width as usize + 4)
                    .collect::<String>()
            })
            .collect();
        let expected = if width == 40 {
            [
                "╭ format request ──────────────────╮",
                "│   #  Heading                     │",
                "│   B  Bold                        │",
                "│   I  Italic                      │",
                "│   •  Bulleted list               │",
                "│   ☐  Checklist                   │",
                "│   `  Inline code                 │",
                "│  {}  Code block                  │",
                "├──────────────────────────────────┤",
                "│ ↑↓\u{a0}select  ↵\u{a0}apply  esc\u{a0}cancel   │",
                "╰──────────────────────────────────╯",
            ]
        } else {
            [
                "╭ format request ──────────────────────╮",
                "│   #  Heading                         │",
                "│   B  Bold                            │",
                "│   I  Italic                          │",
                "│   •  Bulleted list                   │",
                "│   ☐  Checklist                       │",
                "│   `  Inline code                     │",
                "│  {}  Code block                      │",
                "├──────────────────────────────────────┤",
                "│ ↑↓\u{a0}select  ↵\u{a0}apply  esc\u{a0}cancel       │",
                "╰──────────────────────────────────────╯",
            ]
        };
        assert_eq!(captured, expected);
    }
}

#[test]
fn formatting_shortcuts_leave_chat_and_existing_ctrl_f_editing_unchanged() {
    let (_data, mut app) = editor();
    commit_project(&app.engine.project);
    paste(&mut app, "abcdef");
    key(&mut app, KeyCode::Home, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert_eq!(app.input.cursor(), (0, 1));
    assert_eq!(app.input.lines(), ["abcdef"]);
    assert!(app.formatting_menu.is_none());

    key(&mut app, KeyCode::End, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    paste(&mut app, "/Keep my chat draft");
    key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
    assert_eq!(app.input.lines(), ["/Keep my chat draft"]);
    assert!(app.formatting_menu.is_none());
    assert_eq!(app.current_mod().unwrap().draft, "/Keep my chat draft");
}
