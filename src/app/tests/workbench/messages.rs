use super::*;

fn long_message_app() -> (TestData, App, String) {
    let (data, mut app) = reviewed_app();
    let prose =
        "Preserve the saved files and verify the complete result before continuing. ".repeat(9);
    let payload = format!(
        "[\n{}\n]\n",
        (0..323)
            .map(|i| format!(
                "  {{\"task\": {i}, \"outcome\": \"Preserve the saved source and checks\"}}{}",
                if i < 322 { "," } else { "" }
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let body = format!("{prose}\n\n{payload}After all checks pass.");
    app.engine.mods[0].messages = vec![crate::store::Message {
        source: None,
        task: None,
        item_id: None,
        role: "user".into(),
        body: body.clone(),
        model: None,
        effort: None,
    }];
    (data, app, body)
}

#[test]
fn message_controls_expand_prose_and_source_independently_with_keyboard_and_mouse() {
    let (_data, mut app, body) = long_message_app();
    let collapsed = rows(&overview_screen(&mut app, 116, 42)).join("\n");
    assert!(
        collapsed.contains("> You") && collapsed.contains("Show full message"),
        "{collapsed}"
    );
    assert!(
        collapsed.contains("JSON · 325 lines") && collapsed.contains("Show source"),
        "{collapsed}"
    );
    assert!(!collapsed.contains("\"task\""), "{collapsed}");
    let prose = app.workspace.message_controls[0];
    let source = app.workspace.message_controls[1];
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let expanded = rows(&overview_screen(&mut app, 116, 48)).join("\n");
    assert!(
        expanded.contains("Show less") && !expanded.contains("\"task\""),
        "{expanded}"
    );
    assert!(app.workspace.message_toggles.contains(&(prose.0, prose.1)));
    click(&mut app, Control::Message(source.0, source.1));
    let expanded = rows(&overview_screen(&mut app, 116, 48)).join("\n");
    assert!(
        expanded.contains("Hide source") && expanded.contains("\"task\": 0"),
        "{expanded}"
    );
    click(&mut app, Control::Message(source.0, source.1));
    let collapsed = rows(&overview_screen(&mut app, 116, 48)).join("\n");
    assert!(
        collapsed.contains("Show less") && collapsed.contains("Show source"),
        "{collapsed}"
    );
    assert_eq!(app.input.lines(), ["keep this draft"]);
    assert_eq!(app.engine.mods[0].messages[0].body, body);
    let snapshot = serde_json::to_value(app.engine.snapshot()).unwrap();
    assert_eq!(snapshot["active_mod"]["messages"][0]["body"], body);
    assert_eq!(
        snapshot["active_mod"]["messages"][0]["content"]["segments"][1]["source"]
            .as_str()
            .unwrap()
            .lines()
            .count(),
        325
    );
    assert!(app.view == View::Chat);
}

#[test]
fn message_state_follows_identity_through_reorder_and_history_and_resets_for_another_codemod() {
    let (_data, mut app, _) = long_message_app();
    overview_screen(&mut app, 116, 48);
    let source = app.workspace.message_controls[1];
    click(&mut app, Control::Message(source.0, source.1));
    let mut message = app.engine.mods[0].messages[0].clone();
    message.role = "planner".into();
    app.engine.mods[0].messages.insert(0, message.clone());
    overview_screen(&mut app, 116, 48);
    assert!(
        app.workspace
            .message_toggles
            .contains(&(source.0, source.1))
    );
    let original = app.engine.mods[0].messages.remove(1);
    app.engine.mods[0].messages.insert(0, original);
    overview_screen(&mut app, 116, 48);
    assert!(
        app.workspace
            .message_toggles
            .contains(&(source.0, source.1))
    );
    app.workspace.history = true;
    overview_screen(&mut app, 116, 48);
    let planner_content = crate::message_content::messages(&app.engine.mods[0].messages)[1].key;
    assert!(
        !app.workspace
            .message_toggles
            .contains(&(planner_content, source.1))
    );
    app.workspace.reset_for((999, "different-codemod".into()));
    assert!(app.workspace.message_toggles.is_empty());
    assert!(app.workspace.message_cursor.is_none());
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn long_messages_wrap_at_readable_width_and_keep_theme_and_controls_at_narrow_sizes() {
    for appearance in [
        crate::appearance::Appearance::Light,
        crate::appearance::Appearance::Dark,
    ] {
        for width in [60, 116, 160] {
            let (_data, mut app, _) = long_message_app();
            app.engine.appearance = appearance;
            let buffer = overview_screen(&mut app, width, 48);
            let text = rows(&buffer).join("\n");
            assert!(
                text.contains("Show full message") && text.contains("JSON · 325 lines"),
                "{width}: {text}"
            );
            let controls = app.workspace.message_controls.clone();
            click(&mut app, Control::Message(controls[1].0, controls[1].1));
            let buffer = overview_screen(&mut app, width, 48);
            let rendered = rows(&buffer);
            assert!(
                rendered.iter().any(|line| line.contains("\"task\": 0")),
                "{rendered:?}"
            );
            let row = rendered
                .iter()
                .position(|line| line.contains("\"task\": 0"))
                .unwrap();
            let column = rendered[row].chars().position(|c| c == '"').unwrap();
            let key_color = buffer[(column as u16, row as u16)].fg;
            let expected = if appearance == crate::appearance::Appearance::Light {
                ratatui::style::Color::Rgb(48, 102, 141)
            } else {
                ratatui::style::Color::Rgb(155, 197, 226)
            };
            assert_eq!(key_color, expected);
            assert_eq!(app.input.lines(), ["keep this draft"]);
        }
    }
}
