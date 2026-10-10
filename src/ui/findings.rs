use super::*;

pub(super) fn draw(frame: &mut Frame, app: &mut App, scroll: u16, area: Rect) {
    let actions = app.action_dock();
    let available = |action| actions.actions.iter().any(|item| item.action == action);
    let mut keys = vec![("↑↓", "scroll")];
    if available(Action::FixIssues) {
        keys.push(("x", "fix issues"));
    }
    if available(Action::Review) {
        keys.push(("ctrl+e", "review again"));
    }
    if available(Action::Retry) {
        keys.push(("ctrl+r", "retry work"));
    }
    keys.extend([("h", "all history"), ("esc", "back")]);
    let inner = tasks::panel(frame, area, "review findings".into(), &keys);
    let lines = finding_lines(app, inner.width.saturating_sub(2));
    let (scroll, _) = draw_text(
        frame,
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        inner,
        scroll,
        true,
    );
    app.scroll.content = inner;
    app.page_size = inner.height.max(1);
    app.view = View::Findings(scroll);
}

pub(super) fn finding_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(m) = app.current_mod()
        && let Some(review) = &m.agent_review
        && let Some(report) = &review.report
    {
        let outdated = review.status == "stale" || review.status != "fixing" && !review.current(m);
        let status = if review.fixes_verified(m) {
            "Fixes checked · review again to confirm the result.".into()
        } else if outdated {
            "Outdated · review again after current work passes checks.".into()
        } else if review.status == "findings" {
            "Issues found · choose Fix issues to start repairs.".into()
        } else {
            review.label()
        };
        lines.push(Line::from(status).fg(ACCENT).bold());
        lines.push(Line::from(if review.status == "fixing" {
            format!("Previous review: {}", report.summary)
        } else {
            report.summary.clone()
        }));
        for (index, finding) in report.findings.iter().enumerate() {
            let run = m
                .execution
                .as_ref()
                .and_then(|e| e.tasks.iter().find(|run| run.task_id == finding.owner));
            let state = if outdated {
                "Outdated"
            } else if review.status == "fixing" {
                match run.map(|run| run.status.as_str()) {
                    Some("done") => "Awaiting fresh review",
                    Some("sending" | "running" | "checking") => "Fixing / checking",
                    Some("blocked" | "paused" | "repair_paused") => "Fix paused",
                    _ => "Fix queued",
                }
            } else {
                "Open"
            };
            lines.extend(finding_block(index, finding, state, width));
        }
        if report.status == "clean" {
            lines.push(Line::default());
            lines.push(Line::from("No issues found in the reviewed version.").fg(KEY_HINT));
        }
    } else if let Some(review) = app.current_mod().and_then(|m| m.agent_review.as_ref()) {
        lines.push(Line::from(review.label()).fg(ACCENT));
    } else {
        lines.push(Line::from("No review findings yet."));
    }
    lines
}

fn saved_findings(text: &str) -> Option<Vec<crate::review::Finding>> {
    // Saved feedback includes worker instructions after the JSON value.
    let json = text.trim().strip_prefix("Independent review findings:")?;
    serde_json::Deserializer::from_str(json.trim_start())
        .into_iter::<Vec<crate::review::Finding>>()
        .next()?
        .ok()
        .filter(|findings| !findings.is_empty())
}

pub(super) fn saved_summary(text: &str) -> String {
    saved_findings(text).map_or_else(
        || text.lines().next().unwrap_or(text).to_owned(),
        |findings| format!("{} review findings · {}", findings.len(), findings[0].title),
    )
}

pub(super) fn saved_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    if let Some(findings) = saved_findings(text) {
        let mut lines =
            vec![Line::from(format!("Saved review · {} findings", findings.len())).fg(KEY_HINT)];
        for (index, finding) in findings.iter().enumerate() {
            lines.extend(finding_block(index, finding, "", width));
        }
        lines
    } else {
        let mut lines = vec![Line::from("Saved failure").fg(KEY_HINT).bold()];
        lines.extend(indented_text(text, width, 2));
        lines
    }
}

fn finding_block(
    index: usize,
    finding: &crate::review::Finding,
    state: &str,
    width: u16,
) -> Vec<Line<'static>> {
    let color = match finding.priority.as_str() {
        "P0" | "P1" => Color::Red,
        "P2" => Color::Yellow,
        _ => KEY_HINT,
    };
    let mut lines = vec![Line::default()];
    lines.extend(wrap_line(
        Line::from(vec![
            format!("{}. ", index + 1).fg(KEY_HINT),
            format!(" {} ", finding.priority)
                .fg(color)
                .bg(CONTROL)
                .bold(),
            format!(" {}", finding.title).bold(),
        ]),
        width,
        3,
    ));
    lines.extend(wrap_line(
        Line::from(format!(
            "   {}:{} · {}",
            finding.file, finding.line, finding.owner
        ))
        .fg(KEY_HINT),
        width,
        3,
    ));
    if !state.is_empty() {
        lines.push(Line::from(format!("   {state}")).fg(KEY_HINT));
    }
    for (label, body) in [
        ("Evidence", &finding.evidence),
        ("Proposed fix", &finding.fix),
    ] {
        lines.push(Line::default());
        lines.push(Line::from(format!("   {label}")).fg(ACCENT).bold());
        lines.extend(indented_text(body, width, 3));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_findings_keep_evidence_and_ignore_worker_instructions() {
        let finding = serde_json::json!({
            "owner":"database", "file":"app/static/todo-store.mjs", "line":178,
            "priority":"P1", "title":"Restore the change sequence",
            "evidence":"After reopening, sequence [4] becomes 0.\nSurviving tabs reject the older result.",
            "fix":"Persist and restore the change sequence before accepting requests."
        });
        let feedback = format!(
            "Independent review findings:\n[{finding}]\nAddress only these defects within your scope."
        );
        for width in [32, 64, 100] {
            let lines = saved_lines(&feedback, width);
            assert!(lines.iter().all(|line| line.width() <= width as usize));
            let text = lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("Evidence") && text.contains("Proposed fix"));
            let unwrapped = text.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(unwrapped.contains(
                "After reopening, sequence [4] becomes 0. Surviving tabs reject the older result."
            ));
            assert!(!text.contains("Address only") && !text.contains("\"owner\""));
            assert!(
                lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .any(|span| span.content == " P1 " && span.style.fg == Some(Color::Red))
            );
        }
        for text in [
            "A plain failure with evidence.",
            "Independent review findings: [invalid",
        ] {
            assert!(saved_findings(text).is_none());
            assert!(
                saved_lines(text, 100)
                    .iter()
                    .any(|line| line.to_string().contains(text))
            );
        }
    }
}
