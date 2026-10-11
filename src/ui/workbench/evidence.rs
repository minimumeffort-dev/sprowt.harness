use super::*;

pub(super) struct Document {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) controls: Vec<(usize, Control)>,
    width: u16,
}

impl Document {
    fn new(width: u16) -> Self {
        Self {
            lines: Vec::new(),
            controls: Vec::new(),
            width,
        }
    }
    pub(super) fn message(text: &str, width: u16) -> Self {
        let mut d = Self::new(width);
        d.text(text);
        d
    }
    fn text(&mut self, text: &str) {
        self.lines.extend(indented_text(text, self.width, 0));
    }
    fn gap(&mut self) {
        self.lines.push(Line::default());
    }
    fn heading(&mut self, text: &str) {
        self.lines.extend(wrap_line(
            Line::from(text.to_owned()).fg(ACCENT).bold(),
            self.width,
            0,
        ));
    }
    fn muted(&mut self, text: &str) {
        self.lines.extend(wrap_line(
            Line::from(text.to_owned()).fg(KEY_HINT),
            self.width,
            0,
        ));
    }
    fn control(&mut self, label: String, control: Control, selected: bool) {
        self.controls.push((self.lines.len(), control));
        self.lines.push(
            Line::from(fit_name(&format!(" {label} "), self.width))
                .fg(ACCENT)
                .bg(if selected { SELECTED } else { CONTROL }),
        );
    }
}

pub(super) fn document(app: &App, item: &Item, width: u16) -> Document {
    let mut d = Document::new(width);
    match app.timeline.inspector.tab {
        EvidenceTab::Summary => {
            if let Some(question) = app.current_mod().and_then(|m| m.question()) {
                d.heading(&format!("Answer #{}", question.id));
                d.text(&question.body);
                d.muted("Answer in the composer below.");
                d.gap();
            }
            summary(app, item, &mut d);
        }
        EvidenceTab::Checks => checks(app, item, &mut d, true),
        EvidenceTab::Changes => changes(app, item, &mut d),
        EvidenceTab::Activity => activity(app, item, &mut d),
    }
    d
}

fn summary(app: &App, item: &Item, d: &mut Document) {
    if item.key == Key::Review {
        review(app, d);
        return;
    }
    if item.key == Key::Checks {
        checks(app, item, d, false);
        return;
    }
    if let Key::Task(id) | Key::Repair(id) = item.key {
        task(app, id, d);
        return;
    }
    d.heading(if item.key == Key::Request {
        "Requested outcome"
    } else {
        &item.title
    });
    d.gap();
    if item.key == Key::Request {
        d.text(&item.title);
        return;
    }
    for detail in &item.detail {
        d.text(detail);
        d.gap();
    }
    let Some(plan) = app
        .current_mod()
        .and_then(|m| m.planning.as_ref())
        .and_then(|p| p.plan.as_ref())
    else {
        return;
    };
    match item.key {
        Key::Build | Key::Plan => {
            for (i, planned) in plan.tasks.iter().enumerate() {
                let run = app
                    .current_mod()
                    .and_then(|m| m.execution.as_ref())
                    .and_then(|e| e.tasks.iter().find(|r| r.task_id == planned.id));
                let task = run.and_then(|r| app.inspect_task(r.id));
                let key = run.map_or(Key::Planned(i), |r| {
                    if r.status != "done"
                        && (r.check_repair
                            || r.review_feedback.is_some()
                            || r.conflict.is_some()
                            || !r.verification_feedback.is_empty())
                    {
                        Key::Repair(r.id)
                    } else {
                        Key::Task(r.id)
                    }
                });
                d.control(
                    format!("{}. {}", i + 1, planned.title),
                    Control::Drill(key),
                    false,
                );
                let state = task.as_ref().map_or("Waiting", |r| r.state);
                let color = task.as_ref().map_or(KEY_HINT, |r| {
                    if r.error.is_some() {
                        Color::Red
                    } else if matches!(r.state, "Done" | "Working" | "Checking") {
                        ACCENT
                    } else {
                        KEY_HINT
                    }
                });
                d.lines.extend(wrap_line(
                    Line::from(format!("   {state}")).fg(color),
                    d.width,
                    3,
                ));
                d.text(&format!("   {}", planned.outcome));
                d.gap();
            }
            if item.key == Key::Build {
                let open = app
                    .timeline
                    .expanded
                    .get(&Key::Build)
                    .copied()
                    .unwrap_or(false);
                d.control(
                    format!(
                        "space {} tasks in timeline",
                        if open { "Hide" } else { "Show" }
                    ),
                    Control::Expand,
                    false,
                );
            }
            d.control(
                format!(
                    "e {} plan contracts",
                    if app.show_evidence { "Hide" } else { "Show" }
                ),
                Control::Evidence,
                false,
            );
            if app.show_evidence {
                for (label, notes) in [
                    ("Contracts", &plan.contracts),
                    ("Assumptions", &plan.assumptions),
                ] {
                    if !notes.is_empty() {
                        d.gap();
                        d.heading(label);
                        for note in notes {
                            d.text(&format!("  · {note}"));
                        }
                    }
                }
            }
        }
        Key::Environment => {
            if let Some(recipe) = &plan.environment {
                for (label, commands) in [
                    ("Setup", &recipe.setup),
                    ("Capabilities", &recipe.capabilities),
                    ("Baseline", &recipe.baseline),
                ] {
                    if commands.is_empty() {
                        continue;
                    }
                    d.heading(label);
                    for command in commands {
                        d.text(&format!("  · {}", command.name));
                    }
                    d.gap();
                }
            }
            d.control(
                "↵ Inspect preparation receipts".into(),
                Control::FullEvidence,
                false,
            );
        }
        Key::Planned(index) => {
            if let Some(task) = plan.tasks.get(index) {
                d.heading("Success criteria");
                for check in &task.checks {
                    d.text(&format!("  · {check}"));
                }
            }
        }
        Key::Failure(_) => {
            d.lines.clear();
            d.heading(&item.title);
            for detail in &item.detail {
                d.lines.extend(findings::saved_lines(detail, d.width));
            }
        }
        _ => {}
    }
}

fn task(app: &App, id: i64, d: &mut Document) {
    let Some(task) = app.inspect_task(id) else {
        return;
    };
    d.heading(&task.task.title);
    d.muted(&task.identity);
    d.gap();
    d.lines.push(
        Line::from(format!(" {} ", task.state))
            .bold()
            .fg(if task.error.is_some() {
                Color::Red
            } else {
                ACCENT
            })
            .bg(CONTROL),
    );
    d.gap();
    d.text(&task.task.outcome);
    if !task.note.is_empty() {
        d.gap();
        d.text(&task.note);
    }
    if let Some(error) = task.error.filter(|error| *error != task.note) {
        d.gap();
        d.heading("Needs attention");
        d.text(error);
    }
    if let Some(worker) = app.active_workers().find(|w| w.task_run_id() == Some(id)) {
        d.gap();
        d.muted(&worker.timeline_timing());
        if let Some(progress) = worker.progress() {
            d.text(progress);
        }
    }
    if let Some(check) = task.checks.iter().find(|c| c.failed()) {
        d.gap();
        d.heading("Failed check");
        d.text(&check.check);
        d.text(&check.evidence());
    }
    if let Some(latest) = task.latest {
        d.gap();
        d.heading("Latest update");
        let brief = latest.body.lines().take(4).collect::<Vec<_>>().join("\n");
        d.text(if app.show_evidence {
            &latest.body
        } else {
            &brief
        });
    }
    d.gap();
    d.control(
        format!("2 Checks · {} recorded commands", task.checks.len()),
        Control::Tab(EvidenceTab::Checks),
        false,
    );
    d.gap();
    d.control(
        "e Full update / evidence".into(),
        Control::Evidence,
        app.show_evidence,
    );
    d.gap();
    d.control("↵ Full task details".into(), Control::FullEvidence, false);
}

fn review(app: &App, d: &mut Document) {
    let Some(m) = app.current_mod() else { return };
    let Some(review) = &m.agent_review else {
        d.heading("Review pending");
        d.text("Review starts after the combined checks pass.");
        return;
    };
    d.heading(&review.label());
    let current = review.current(m);
    d.muted(if current {
        "Current source"
    } else {
        "Saved review · changes need a fresh review"
    });
    if !review.issues.is_empty() {
        d.muted(&review.progress());
    }
    if let Some(halt) = &review.halt {
        d.gap();
        d.text(halt);
    }
    let Some(report) = &review.report else { return };
    d.gap();
    d.text(&report.summary);
    let entries: Vec<_> = if review.issues.is_empty() {
        report
            .findings
            .iter()
            .enumerate()
            .map(|(i, f)| (f.id.unwrap_or(i as u32 + 1), f, "Open"))
            .collect()
    } else {
        review
            .ordered_issues()
            .map(|issue| (issue.id, &issue.finding, issue.status.as_str()))
            .collect()
    };
    if entries.is_empty() {
        return;
    }
    d.gap();
    let index = entries
        .iter()
        .position(|(id, _, _)| Some(*id) == app.timeline.inspector.finding)
        .unwrap_or(0);
    for (i, (id, finding, _)) in entries.iter().enumerate() {
        let color = if matches!(finding.priority.as_str(), "P0" | "P1") {
            Color::Red
        } else {
            Color::Yellow
        };
        d.controls.push((d.lines.len(), Control::Finding(*id)));
        d.lines.push(
            Line::from(vec![
                format!(" R{id} ").fg(KEY_HINT),
                format!("{} ", finding.priority).fg(color).bold(),
                fit_name(&finding.title, d.width.saturating_sub(10)).fg(if i == index {
                    ACCENT
                } else {
                    Color::Reset
                }),
            ])
            .bg(if i == index { SELECTED } else { Color::Reset }),
        );
        d.gap();
    }
    if index > 0 {
        d.control(
            "[ Previous finding".into(),
            Control::Finding(entries[index - 1].0),
            false,
        );
    }
    if index + 1 < entries.len() {
        d.control(
            "] Next finding".into(),
            Control::Finding(entries[index + 1].0),
            false,
        );
    }
    d.gap();
    let (id, finding, state) = entries[index];
    d.heading(&format!("R{id} · {}", finding.title));
    d.muted(&format!(
        "{}:{} · {}",
        finding.file, finding.line, finding.owner
    ));
    d.muted(if current {
        state
    } else {
        "Awaiting fresh review"
    });
    d.gap();
    d.heading("Observed evidence");
    d.text(&finding.evidence);
    d.gap();
    d.control(
        "e Acceptance & proposed fix".into(),
        Control::Evidence,
        app.show_evidence,
    );
    if app.show_evidence {
        d.gap();
        d.heading("Proposed fix");
        d.text(&finding.fix);
        if let Some(triage) = &finding.triage {
            d.gap();
            d.heading("Acceptance");
            d.text(&triage.acceptance);
            d.gap();
            d.heading("Reproduction");
            d.text(&triage.reproduction);
        }
    }
}

fn checks(app: &App, item: &Item, d: &mut Document, detailed: bool) {
    let task = match item.key {
        Key::Task(id) | Key::Repair(id) => app.inspect_task(id),
        _ => None,
    };
    if let Some(task) = task {
        d.heading(&format!("{} · {}", task.task.title, task.checks_label));
        d.gap();
        if task.checks.is_empty() {
            for expected in &task.task.checks {
                d.text(&format!("  ○ {expected} · not run"));
            }
        } else {
            d.lines.extend(inspector::check_lines(
                &task.checks,
                d.width,
                app.show_evidence,
            ));
        }
    } else if detailed {
        d.lines = inspector::check_summary(app, d.width);
    } else if let Some(e) = app.current_mod().and_then(|m| m.execution.as_ref()) {
        d.heading(if e.checks.iter().any(|check| check.failed()) {
            "Combined checks · Needs attention"
        } else {
            "Combined checks"
        });
        d.muted(
            if e.status == "review" && e.fingerprint.is_some() && e.complete() {
                "Results for the verified version"
            } else if e.status == "verifying" {
                "Combined verification is running"
            } else {
                "Saved results · this version is not yet verified"
            },
        );
        d.gap();
        d.text(&format!(
            "{} passed · {} failed · {} not run",
            e.checks.iter().filter(|c| c.exit_code == Some(0)).count(),
            e.checks.iter().filter(|c| c.failed()).count(),
            e.checks.iter().filter(|c| c.skipped()).count()
        ));
        for check in e.checks.iter().filter(|c| c.failed()) {
            d.gap();
            d.lines.extend(wrap_line(
                Line::from(format!("! {}", check.check))
                    .fg(Color::Red)
                    .bold(),
                d.width,
                2,
            ));
            d.text(&check.evidence());
            if let Some(timing) = check.timing() {
                d.muted(&timing);
            }
            if app.show_evidence {
                d.lines.extend(command_lines(&check.command, d.width));
                d.text(&check.output);
            }
        }
        d.gap();
        d.control(
            "2 Inspect all checks".into(),
            Control::Tab(EvidenceTab::Checks),
            false,
        );
    }
    d.gap();
    d.control(
        "e Commands & raw output".into(),
        Control::Evidence,
        app.show_evidence,
    );
}

fn changes(app: &App, item: &Item, d: &mut Document) {
    d.heading("Changes");
    d.gap();
    let dock = app.action_dock();
    if dock.actions.iter().any(|a| a.action == Action::Diff) {
        d.control(
            "ctrl+d Load current diff".into(),
            Control::Action(Action::Diff),
            false,
        );
        d.gap();
        d.muted("Reads the current working files in the full diff view.");
    } else {
        d.text(
            if dock.actions.iter().any(|item| item.action == Action::Stop) {
                "Pause the worker to inspect its changes."
            } else {
                "Current diff unavailable."
            },
        );
    }
    let Some(plan) = app
        .current_mod()
        .and_then(|m| m.planning.as_ref())
        .and_then(|p| p.plan.as_ref())
    else {
        return;
    };
    let task = match item.key {
        Key::Task(id) | Key::Repair(id) => app.inspect_task(id).map(|t| t.task),
        Key::Planned(index) => plan.tasks.get(index),
        _ => None,
    };
    let mut paths = std::collections::BTreeSet::new();
    for task in task.into_iter().chain(if task.is_none() {
        plan.tasks.iter().collect::<Vec<_>>()
    } else {
        Vec::new()
    }) {
        for file in &task.files {
            paths.insert(file);
        }
    }
    if !paths.is_empty() {
        d.gap();
        d.heading("Planned file scope");
        d.muted("Scope from the plan; not a list of confirmed changes.");
        d.gap();
        for path in paths {
            d.text(&format!("  {path}"));
        }
    }
}

fn activity(app: &App, item: &Item, d: &mut Document) {
    d.heading("Activity");
    d.muted("Saved messages · latest first");
    let Some(m) = app.current_mod() else { return };
    let task = match item.key {
        Key::Task(id) | Key::Repair(id) | Key::Failure(id) => Some(id),
        _ => None,
    };
    let messages: Vec<_> = m
        .messages
        .iter()
        .rev()
        .filter(|message| task.is_none() || message.task == task)
        .collect();
    if messages.is_empty() {
        d.gap();
        d.text("No activity recorded for this step.");
    }
    for message in messages
        .into_iter()
        .take(if app.show_evidence { usize::MAX } else { 6 })
    {
        d.gap();
        d.heading(&message.role);
        let brief = message.body.lines().take(3).collect::<Vec<_>>().join("\n");
        d.text(if app.show_evidence {
            &message.body
        } else {
            &brief
        });
    }
    d.gap();
    d.control(
        "e Full saved activity".into(),
        Control::Evidence,
        app.show_evidence,
    );
}
