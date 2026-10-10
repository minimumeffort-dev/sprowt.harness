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
    let Some(m) = app.current_mod() else {
        return lines;
    };
    if let Some(e) = &m.execution {
        lines.push(Line::from("Combined verification").fg(ACCENT).bold());
        lines.extend(wrap_line(
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
            width,
            0,
        ));
        if !e.checks.is_empty() {
            let mut counts = Vec::new();
            for (count, label, color) in [
                (
                    e.checks.iter().filter(|c| c.exit_code == Some(0)).count(),
                    "passed",
                    ACCENT,
                ),
                (
                    e.checks.iter().filter(|c| c.failed()).count(),
                    "failed",
                    Color::Red,
                ),
                (
                    e.checks.iter().filter(|c| c.skipped()).count(),
                    "not run",
                    KEY_HINT,
                ),
            ] {
                if count > 0 {
                    if !counts.is_empty() {
                        counts.push(" · ".fg(KEY_HINT));
                    }
                    counts.push(format!("{count} {label}").fg(color));
                }
            }
            lines.extend(wrap_line(Line::from(counts), width, 0));
            let mut owners = Vec::new();
            for check in &e.checks {
                if !owners.contains(&check.task) {
                    owners.push(check.task);
                }
            }
            for (phase, label, color) in [
                (0, "Needs attention", Color::Red),
                (1, "Passed checks", ACCENT),
                (2, "Not run", KEY_HINT),
            ] {
                let phase_checks: Vec<_> =
                    e.checks
                        .iter()
                        .filter(|check| {
                            let group = e.checks.iter().filter(|other| {
                                other.task == check.task && other.check == check.check
                            });
                            let state = if group.clone().any(|c| c.failed()) {
                                0
                            } else if group.clone().all(|c| c.exit_code == Some(0)) {
                                1
                            } else {
                                2
                            };
                            state == phase
                        })
                        .collect();
                if phase_checks.is_empty() {
                    continue;
                }
                lines.push(Line::default());
                lines.push(Line::from(label).fg(color).bold());
                for owner in &owners {
                    let checks: Vec<_> = phase_checks
                        .iter()
                        .copied()
                        .filter(|check| check.task == *owner)
                        .collect();
                    if checks.is_empty() {
                        continue;
                    }
                    let title = owner.and_then(|id| app.inspect_task(id)).map_or_else(
                        || "Saved checks".to_owned(),
                        |task| format!("{}. {}", task.number, task.task.title),
                    );
                    lines.extend(wrap_line(Line::from(title).bold(), width, 0));
                    if phase == 2 && !app.show_evidence && checks.iter().all(|c| c.skipped()) {
                        lines.extend(wrap_line(
                            Line::from(format!(
                                "  · {} command{} not run · e show evidence",
                                checks.len(),
                                if checks.len() == 1 { "" } else { "s" }
                            ))
                            .fg(KEY_HINT),
                            width,
                            4,
                        ));
                        lines.push(Line::default());
                    } else {
                        lines.extend(check_lines(&checks, width, app.show_evidence));
                    }
                }
            }
        }
    }
    for id in app.task_ids() {
        if let Some(task) = app.inspect_task(id) {
            if m.execution
                .as_ref()
                .is_some_and(|e| e.checks.iter().any(|c| c.task == Some(id)))
            {
                continue;
            }
            lines.push(Line::default());
            lines.extend(wrap_line(
                Line::from(format!(
                    "{}. {} · {}",
                    task.number, task.task.title, task.state
                ))
                .bold(),
                width,
                0,
            ));
            if task.checks.is_empty() {
                for expected in &task.task.checks {
                    lines.extend(wrap_line(
                        Line::from(format!("  · {expected} · not run")).fg(KEY_HINT),
                        width,
                        4,
                    ));
                }
            } else {
                lines.push(
                    Line::from(
                        if task.run.checks.is_empty() && !task.run.verification_feedback.is_empty()
                        {
                            "Previous failed checks"
                        } else {
                            "Task verification · before combined checks"
                        },
                    )
                    .fg(KEY_HINT),
                );
                lines.extend(check_lines(&task.checks, width, app.show_evidence));
            }
        }
    }
    if app.task_ids().is_empty() {
        lines.push(Line::from("No checks planned yet."));
    }
    lines
}

fn check_lines(
    checks: &[&crate::execution::CheckResult],
    width: u16,
    evidence: bool,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut names = Vec::new();
    for check in checks {
        if !names.contains(&check.check.as_str()) {
            names.push(check.check.as_str());
        }
    }
    for name in names {
        let group: Vec<_> = checks
            .iter()
            .copied()
            .filter(|check| check.check == name)
            .collect();
        let failed = group.iter().any(|check| check.failed());
        let passed = group.iter().all(|check| check.exit_code == Some(0));
        let (glyph, color) = if failed {
            ("!", Color::Red)
        } else if passed {
            ("✓", ACCENT)
        } else {
            ("·", KEY_HINT)
        };
        lines.extend(wrap_line(
            Line::from(vec![
                format!("  {glyph} ").fg(color).bold(),
                Span::raw(name.to_owned()),
            ]),
            width,
            4,
        ));
        if group.len() > 1 {
            let status = if passed {
                "passed".to_owned()
            } else {
                format!(
                    "{} passed · {} failed · {} not run",
                    group.iter().filter(|c| c.exit_code == Some(0)).count(),
                    group.iter().filter(|c| c.failed()).count(),
                    group.iter().filter(|c| c.skipped()).count()
                )
            };
            lines.extend(wrap_line(
                Line::from(format!("    {} commands · {}", group.len(), status)).fg(color),
                width,
                4,
            ));
        }
        for check in group {
            if (check.failed() || evidence)
                && let Some(timing) = check.timing()
            {
                lines.extend(wrap_line(
                    Line::from(format!("    {timing}")).fg(KEY_HINT),
                    width,
                    4,
                ));
            }
            if evidence {
                lines.extend(command_lines(&check.command, width));
            }
            let output = if evidence {
                check.output.clone()
            } else if check.failed() {
                check.evidence()
            } else {
                String::new()
            };
            if !output.is_empty() {
                lines.extend(
                    indented_text(&output, width, 4)
                        .into_iter()
                        .map(|line| line.fg(if check.failed() { Color::Red } else { KEY_HINT })),
                );
            }
        }
        lines.push(Line::default());
    }
    lines
}
