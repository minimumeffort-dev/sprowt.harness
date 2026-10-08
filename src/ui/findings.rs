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
    let mut lines = Vec::new();
    if let Some(m) = app.current_mod()
        && let Some(review) = &m.agent_review
        && let Some(report) = &review.report
    {
        let outdated = review.status == "stale" || review.status != "fixing" && !review.current(m);
        let status = if outdated {
            "Outdated · review again after current work passes checks.".into()
        } else if review.status == "findings" {
            if review.rounds >= 2 {
                "Fix limit reached · send edits to revise the plan.".into()
            } else {
                "Issues found · choose Fix issues to start repairs.".into()
            }
        } else {
            review.label()
        };
        lines.push(Line::from(status).fg(ACCENT).bold());
        lines.push(Line::from(report.summary.clone()));
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
            lines.push(Line::default());
            lines.push(
                Line::from(format!(
                    "{}. {} · {}",
                    index + 1,
                    finding.priority,
                    finding.title
                ))
                .bold(),
            );
            lines.push(
                Line::from(format!(
                    "{state} · {}:{} · {}",
                    finding.file, finding.line, finding.owner
                ))
                .fg(KEY_HINT),
            );
            lines.push(Line::from(finding.evidence.clone()));
            lines.push(Line::from(format!("Proposed fix: {}", finding.fix)));
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
    let text = Paragraph::new(lines).wrap(Wrap { trim: false });
    let max_scroll = text
        .line_count(inner.width)
        .saturating_sub(inner.height as usize)
        .min(u16::MAX as usize) as u16;
    let scroll = scroll.min(max_scroll);
    frame.render_widget(text.scroll((scroll, 0)), inner);
    app.scroll.content = inner;
    app.page_size = inner.height.max(1);
    app.view = View::Findings(scroll);
}
