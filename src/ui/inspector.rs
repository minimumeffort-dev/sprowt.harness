use super::*;

pub(super) fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let state = app.state_summary();
    let mut heading = vec![
        Line::from(format!("Details · {}", state.status))
            .fg(ACCENT)
            .bold(),
    ];
    if !state.evidence.is_empty() {
        heading.push(Line::from(state.evidence).fg(KEY_HINT));
    }
    let selected = app.view.details_tab().unwrap_or(0);
    let mut tabs = Vec::new();
    for (i, label) in ["Tasks", "Checks", "Review", "Activity"].iter().enumerate() {
        if i > 0 {
            tabs.push(Span::raw("   "));
        }
        let span = Span::raw(format!("{} {label}", i + 1));
        tabs.push(if i == selected {
            span.fg(ACCENT).bg(CONTROL).bold()
        } else {
            span.fg(KEY_HINT)
        });
    }
    heading.extend(wrap_line(Line::from(tabs), area.width, 0));
    let header = Paragraph::new(heading).wrap(Wrap { trim: false });
    let [header_area, body] = Layout::vertical([
        Constraint::Length(header.line_count(area.width) as u16),
        Constraint::Min(1),
    ])
    .areas(area);
    frame.render_widget(header, header_area);
    match app.view {
        View::Tasks(index) => tasks::draw_tasks(frame, app, index, body),
        View::Task(id, scroll) => tasks::draw_task(frame, app, id, scroll, body),
        View::Checks(scroll) => draw_checks(frame, app, scroll, body),
        View::Findings(scroll) => findings::draw(frame, app, scroll, body),
        View::History(scroll) => draw_history(frame, app, scroll, body, None),
        View::TaskHistory(id, scroll) => draw_history(frame, app, scroll, body, Some(id)),
        _ => {}
    }
}

fn draw_checks(frame: &mut Frame, app: &mut App, scroll: u16, area: Rect) {
    let mut hints = vec![("↑↓", "scroll")];
    if app.failed_check_owner().is_some() {
        hints.push(("x", "Fix failed check"));
    }
    hints.extend([
        (
            "e",
            if app.show_evidence {
                "hide evidence"
            } else {
                "show evidence"
            },
        ),
        ("tab", "section"),
        ("esc", "back"),
    ]);
    let inner = tasks::panel(frame, area, "checks".into(), &hints);
    let lines = check_summary(app, inner.width.saturating_sub(2));
    let (scroll, _) = draw_text(
        frame,
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        inner,
        scroll,
        true,
    );
    app.scroll.content = inner;
    app.page_size = inner.height.max(1);
    app.view = View::Checks(scroll);
}

pub(super) fn check_summary(app: &App, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(m) = app.current_mod() {
        if let Some(e) = &m.execution {
            lines.push(
                Line::from(if e.checks.is_empty() {
                    "Combined checks have not run on this version."
                } else if e.status == "verifying" {
                    "Combined checks are running."
                } else if e.status == "review" && e.fingerprint.is_some() && e.complete() {
                    "Combined results for the verified version."
                } else {
                    "Saved results · this version is not yet verified."
                })
                .fg(KEY_HINT),
            );
            if !e.checks.is_empty() {
                lines.extend(check_lines(&e.checks, width, app.show_evidence));
            }
        }
        lines.push(Line::default());
        for id in app.task_ids() {
            if let Some(task) = app.inspect_task(id) {
                lines.push(
                    Line::from(format!(
                        "{}. {} · {}",
                        task.number, task.task.title, task.state
                    ))
                    .bold(),
                );
                if task.checks.is_empty() {
                    for expected in &task.task.checks {
                        lines.push(Line::from(format!("· {expected} · not run")).fg(KEY_HINT));
                    }
                } else {
                    // Combined results already appear above; task details remain available in Tasks.
                    if !m
                        .execution
                        .as_ref()
                        .is_some_and(|e| e.checks.iter().any(|c| c.task == Some(task.run.id)))
                    {
                        if task.run.checks.is_empty() && !task.run.verification_feedback.is_empty()
                        {
                            lines.push(Line::from("Previous failed checks").fg(KEY_HINT));
                        }
                        lines.extend(check_lines(
                            &task.checks.iter().map(|c| (*c).clone()).collect::<Vec<_>>(),
                            width,
                            app.show_evidence,
                        ));
                    }
                }
                lines.push(Line::default());
            }
        }
        if app.task_ids().is_empty() {
            lines.push(Line::from("No checks planned yet."));
        }
    }
    lines
}

fn check_lines(
    checks: &[crate::execution::CheckResult],
    width: u16,
    evidence: bool,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut ordered: Vec<_> = checks
        .iter()
        .filter(|check| evidence || !check.skipped())
        .collect();
    ordered.sort_by_key(|check| !check.failed());
    for check in ordered {
        let (glyph, status, color) = if check.exit_code == Some(0) {
            ("✓", "passed", KEY_HINT)
        } else if check.failed() {
            ("!", "failed", Color::Red)
        } else {
            ("·", "not run", KEY_HINT)
        };
        lines.push(Line::from(format!("{glyph} {} · {status}", check.check)).fg(color));
        if evidence {
            lines.extend(command_lines(&check.command, width));
            lines.extend(
                check
                    .output
                    .lines()
                    .map(|line| Line::from(line.to_owned()).fg(color)),
            );
        } else if check.failed() {
            lines.push(Line::from(check.brief()).fg(Color::Red));
        }
    }
    let skipped = checks.iter().filter(|check| check.skipped()).count();
    if !evidence && skipped > 0 {
        lines.push(
            Line::from(format!(
                "· {skipped} command{} not run · e show evidence",
                if skipped == 1 { "" } else { "s" }
            ))
            .fg(KEY_HINT),
        );
    }
    lines
}
