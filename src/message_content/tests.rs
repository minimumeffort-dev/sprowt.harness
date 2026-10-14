use super::*;

fn message(body: &str) -> crate::store::Message {
    crate::store::Message {
        source: None,
        task: None,
        item_id: None,
        role: "user".into(),
        body: body.into(),
        model: None,
        effort: None,
    }
}

#[test]
fn recovery_sized_payload_is_separate_from_folded_prose_and_keeps_every_byte() {
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
    assert_eq!(payload.lines().count(), 325);
    assert!(payload.len() > 18000);
    let prose = "Preserve every saved contract and verify the complete source before continuing. "
        .repeat(8);
    let original = format!("{prose}\n\n{payload}\nContinue after the checks pass.");
    let content = messages(&[message(&original)]).remove(0);
    assert_eq!(content.segments.len(), 3);
    let Content::Prose { text, preview } = &content.segments[0].content else {
        panic!("prose")
    };
    assert_eq!(text, prose.trim());
    assert!(preview.as_ref().unwrap().chars().count() <= 360);
    let Content::Code {
        source,
        summary,
        collapsed,
        tokens,
        ..
    } = &content.segments[1].content
    else {
        panic!("code")
    };
    assert_eq!(source, &payload);
    assert!(summary.starts_with("JSON · 325 lines · "));
    assert!(*collapsed);
    assert_eq!(
        tokens
            .iter()
            .map(|token| token.text.as_str())
            .collect::<String>(),
        payload
    );
    let Content::Prose { text, preview } = &content.segments[2].content else {
        panic!("prose")
    };
    assert_eq!(text, "Continue after the checks pass.");
    assert!(preview.is_none());
    assert_eq!(message(&original).body, original);
}

#[test]
fn bare_and_fenced_json_preserve_escapes_indentation_and_surrounding_prose() {
    let raw =
        "  {\"key\": \"界 \\\"quote\\\" \\\\ backslash\", \"number\": -12.5e+2, \"ready\": true}\n";
    let source =
        format!("Before\n{raw}\nBetween\n```json\n[null, false]\n```\nAfter\n{{\"last\":3}}");
    let content = parse(&source);
    assert_eq!(content.len(), 6);
    let Content::Code { source, tokens, .. } = &content[1].content else {
        panic!("code")
    };
    assert_eq!(source, raw);
    assert_eq!(
        tokens
            .iter()
            .map(|token| token.text.as_str())
            .collect::<String>(),
        raw
    );
    assert!(
        tokens
            .iter()
            .any(|token| token.kind == TokenKind::Key && token.text == "\"key\"")
    );
    assert!(
        tokens
            .iter()
            .any(|token| token.kind == TokenKind::Number && token.text == "-12.5e+2")
    );
    let Content::Code { source, .. } = &content[3].content else {
        panic!("code")
    };
    assert_eq!(source, "[null, false]\n");
}

#[test]
fn invalid_json_and_unfinished_fences_remain_literal_without_unsafe_tokenization() {
    for source in [
        "Keep {this} literal.",
        "{not JSON}\nContinue.",
        "[an ordinary label](url)",
        "{\"valid\":true} trailing prose",
        "```json\n{}\nunfinished",
    ] {
        let segments = parse(source);
        assert_eq!(segments.len(), 1);
        let Content::Prose { text, .. } = &segments[0].content else {
            panic!("literal prose")
        };
        assert_eq!(text, source);
    }
    for invalid in [
        "{\"a\": \"unfinished",
        "{\"a\": \"escaped\\",
        "界界",
        "{bad}",
    ] {
        let segments = parse(&format!("```JSON\n{invalid}\n```"));
        let Content::Code { source, tokens, .. } = &segments[0].content else {
            panic!("literal code")
        };
        assert_eq!(source, &format!("{invalid}\n"));
        assert!(tokens.is_empty());
    }
    let segments = parse("{}\n```json\nunfinished");
    assert_eq!(segments.len(), 2);
}

#[test]
fn presentation_identity_survives_reorder_streaming_and_separates_duplicate_requests() {
    let first = message("First message");
    let second = message("Second message");
    let original = messages(&[first.clone(), second.clone(), first.clone()]);
    let reordered = messages(&[second, first.clone(), first]);
    assert_eq!(original[0].id, reordered[1].id);
    assert_eq!(original[1].id, reordered[0].id);
    assert_eq!(original[2].id, reordered[2].id);
    assert_ne!(original[0].id, original[2].id);
    let mut streamed = message("Starting work");
    streamed.item_id = Some("worker-item".into());
    streamed.source = Some("attempt-1".into());
    let start = messages(&[streamed.clone()]).remove(0).id;
    streamed.body = "Starting work and checking files".into();
    assert_eq!(start, messages(&[streamed.clone()]).remove(0).id);
    streamed.source = Some("attempt-2".into());
    assert_ne!(start, messages(&[streamed]).remove(0).id);
}

#[test]
fn unicode_prose_preview_does_not_split_utf8_and_short_code_stays_open() {
    let source = "界面文字文字 ".repeat(90);
    let segments = parse(&source);
    let Content::Prose { preview, .. } = &segments[0].content else {
        panic!("prose")
    };
    assert!(source.starts_with(preview.as_ref().unwrap()));
    let segments = parse("```sh\n  printf '%s' '**literal**'\n```");
    let Content::Code {
        source,
        collapsed,
        tokens,
        ..
    } = &segments[0].content
    else {
        panic!("code")
    };
    assert_eq!(source, "  printf '%s' '**literal**'\n");
    assert!(!collapsed);
    assert!(tokens.is_empty());
}
