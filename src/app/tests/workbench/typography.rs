use super::*;
use ratatui::style::Color;

#[test]
fn task_evidence_aligns_output_and_retains_full_sources_without_repeating_preflight() {
    for width in [80, 160] {
        let (_data, mut app, _, id) = setup_failed_app();
        app.view = View::Task(id, 0);
        app.show_evidence = true;
        let source = app.inspect_task(id).unwrap().run.source.clone();
        let buffer = screen(&mut app, width, 90);
        let text = rows(&buffer).join("\n");
        assert!(text.contains(&source), "{text}");
        assert!(
            text.contains("Source: fixture-source-fingerprint"),
            "{text}"
        );
        assert!(
            text.contains("File scope") && text.contains("RAW_PREPARATION_TAIL"),
            "{text}"
        );
        assert_eq!(
            text.matches("Guest command timed out after 180s.").count(),
            1,
            "{text}"
        );
        assert!(!text.contains("Environment preflight failed:"), "{text}");
        let rendered = rows(&buffer);
        let title = rendered
            .iter()
            .find(|row| row.contains("Saved preparation"))
            .unwrap();
        let output = rendered
            .iter()
            .find(|row| row.contains("RAW_PREPARATION_TAIL"))
            .unwrap();
        assert_eq!(
            title.find("Saved preparation"),
            output.match_indices("│ ").nth(1).map(|(index, _)| index)
        );
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }
}

#[test]
fn preparation_network_request_uses_attention_color_and_preserves_review_action() {
    let (_data, mut app, _, id) = preparation::network_preparation_app();
    app.view = View::Task(id, 0);
    let buffer = screen(&mut app, 160, 70);
    let text = rows(&buffer).join("\n");
    assert!(
        text.contains("Network access pending") && text.contains("Review network access"),
        "{text}"
    );
    let line = rows(&buffer)
        .iter()
        .position(|row| row.contains("Saved preparation"))
        .unwrap() as u16;
    let rendered = &rows(&buffer)[line as usize];
    let x = rendered[..rendered.find("Saved preparation").unwrap()]
        .chars()
        .count() as u16;
    assert_eq!(buffer[(x, line)].fg, Color::Rgb(235, 196, 119));
    assert_eq!(app.action_dock().primary, Some(Action::Network));
    assert_eq!(app.input.lines(), ["keep this draft"]);
}
