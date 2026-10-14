mod agents;
use super::*;
use crate::app::workspace_navigation::Key;

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
    fn text(&mut self, text: &str) {
        self.lines.extend(rich_text::lines(text, self.width));
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

    fn message(
        &mut self,
        app: &App,
        message: &crate::store::Message,
        content: &crate::message_content::MessageContent,
    ) {
        use crate::message_content::Content;
        let start = self.lines.len();
        let width = self.width;
        self.width = width.min(76);
        if message.role == "user" {
            self.lines
                .push(Line::from(vec!["> ".fg(ACCENT), "You".fg(MUTED)]));
        } else {
            self.heading(&message.author());
        }
        for (index, segment) in content.segments.iter().enumerate() {
            if index > 0 {
                self.gap();
            }
            let key = (content.key, segment.id);
            let collapsed =
                segment.initially_collapsed() != app.workspace.message_toggles.contains(&key);
            let selected = app.workspace.message_cursor == Some(key)
                && app.workspace.focus == crate::app::workspace_navigation::Focus::Content;
            match &segment.content {
                Content::Prose { text, preview } => {
                    if collapsed {
                        let mut lines =
                            rich_text::lines(preview.as_ref().unwrap_or(text), self.width);
                        lines.truncate(4);
                        if let Some(last) = lines.last_mut() {
                            *last = Line::from(format!(
                                "{}…",
                                fit_name(&last.to_string(), self.width.saturating_sub(1))
                            ));
                        }
                        self.lines.extend(lines);
                    } else {
                        self.text(text);
                    }
                    if preview.is_some() {
                        self.control(
                            if collapsed {
                                "Show full message"
                            } else {
                                "Show less"
                            }
                            .into(),
                            Control::Message(key.0, key.1),
                            selected,
                        );
                        if !selected {
                            self.lines.last_mut().unwrap().style.bg = None;
                        }
                    }
                }
                Content::Code {
                    source,
                    summary,
                    tokens,
                    ..
                } => {
                    self.lines
                        .push(Line::from("─".repeat(usize::from(self.width))).fg(BORDER));
                    self.control(
                        format!(
                            "{} {summary} · {}",
                            if collapsed { "▸" } else { "▾" },
                            if collapsed {
                                "Show source"
                            } else {
                                "Hide source"
                            }
                        ),
                        Control::Message(key.0, key.1),
                        selected,
                    );
                    if !selected {
                        self.lines.last_mut().unwrap().style.bg = None;
                    }
                    if !collapsed {
                        self.lines.extend(if tokens.is_empty() {
                            rich_text::code(source, self.width)
                        } else {
                            rich_text::tokens(tokens, self.width)
                        });
                    }
                }
            }
        }
        if message.role == "user" {
            for line in self.lines[start..].iter_mut() {
                if line.style.bg.is_none() {
                    line.style.bg = Some(USER_BACKGROUND);
                }
            }
        }
        self.width = width;
    }
}

pub(super) fn document(app: &App, item: Option<Key>, width: u16) -> Document {
    let mut d = Document::new(width);
    if item.is_some() {
        d.control(
            format!("← Back to {}", app.workspace.inspector.tab.label()),
            Control::Back,
            false,
        );
        d.gap();
    }
    match app.workspace.inspector.tab {
        WorkspaceView::Conversation => {
            if let Some(item) = item {
                summary(app, item, &mut d);
            } else {
                conversation(app, &mut d);
            }
        }
        WorkspaceView::Checks => checks(app, item, &mut d),
        WorkspaceView::Changes => changes(app, item, &mut d),
        WorkspaceView::Findings => {
            review(app, &mut d);
            pr_feedback(app, &mut d);
        }
        WorkspaceView::Agents => agents::draw(app, &mut d),
    }
    d
}

fn conversation(app: &App, d: &mut Document) {
    let Some(m) = app.current_mod() else { return };
    let contents = crate::message_content::messages(&m.messages);
    for (message, content) in m
        .messages
        .iter()
        .zip(&contents)
        .filter(|(message, _)| m.shows_conversation_message(message))
    {
        d.message(app, message, content);
        d.gap();
    }
    if let Some(question) = m.question() {
        d.heading(&format!("Answer #{}", question.id));
        d.text(&question.body);
        d.muted("Answer in the composer below.");
        d.gap();
    }
    if let Some(plan) = m
        .planning
        .as_ref()
        .filter(|p| p.status == "ready")
        .and_then(|p| p.plan.as_ref())
        .filter(|p| !p.tasks.is_empty())
    {
        for (index, task) in plan.tasks.iter().enumerate() {
            let run = m
                .execution
                .as_ref()
                .and_then(|e| e.tasks.iter().find(|r| r.task_id == task.id));
            let key = run.map_or(Key::Planned(index), |r| Key::Task(r.id));
            d.control(
                format!("{}. {}", index + 1, task.title),
                Control::Drill(key),
                false,
            );
            d.text(&task.outcome);
            if !task.depends_on.is_empty() {
                let numbers = task
                    .depends_on
                    .iter()
                    .filter_map(|id| plan.tasks.iter().position(|t| &t.id == id))
                    .map(|i| (i + 1).to_string())
                    .collect::<Vec<_>>();
                d.muted(&format!("After: {}", numbers.join(", ")));
            }
            if let Some(run) = run.and_then(|run| app.inspect_task(run.id)) {
                d.lines.extend(wrap_line(
                    Line::from(run.state).fg(theme::status_color(run.state)),
                    d.width,
                    0,
                ));
            }
            d.gap();
        }
        d.control(
            "e Plan details".into(),
            Control::Evidence,
            app.show_evidence,
        );
        if app.show_evidence {
            d.gap();
            for text in plan.contracts.iter().chain(&plan.assumptions) {
                d.text(text);
                d.gap();
            }
        }
        d.gap();
    }
    if m.messages
        .iter()
        .any(|message| !m.shows_conversation_message(message))
    {
        d.control(
            "h Saved history".into(),
            Control::History,
            app.workspace.history,
        );
        if app.workspace.history {
            d.gap();
            for (message, content) in m
                .messages
                .iter()
                .zip(&contents)
                .filter(|(message, _)| !m.shows_conversation_message(message))
            {
                d.message(app, message, content);
                d.gap();
            }
        }
    }
}

fn summary(app: &App, key: Key, d: &mut Document) {
    if let Key::Task(id) = key {
        task(app, id, d);
        return;
    }
    let Key::Planned(index) = key else { return };
    let Some(task) = app
        .current_mod()
        .and_then(|m| m.planning.as_ref())
        .and_then(|p| p.plan.as_ref())
        .and_then(|p| p.tasks.get(index))
    else {
        return;
    };
    d.heading(&task.title);
    d.text(&task.outcome);
    d.gap();
    d.control(
        "e Files, checks and worker".into(),
        Control::Evidence,
        app.show_evidence,
    );
    if app.show_evidence {
        d.gap();
        if !task.files.is_empty() {
            d.heading("File scope");
            d.lines.extend(rich_text::paths(&task.files, d.width));
            d.gap();
        }
        d.heading("Required checks");
        for check in &task.checks {
            d.text(&format!("- {check}"));
        }
    }
}

fn pr_feedback(app: &App, d: &mut Document) {
    let Some(target) = app.current_mod().and_then(|m| app.targets.get(&m.id)) else {
        return;
    };
    if let Some(error) = &target.pr_feedback_error {
        d.gap();
        d.text(&format!("GitHub feedback unavailable · {error}"));
    }
    if !target.pr_feedback.is_empty() {
        d.gap();
        d.heading("GitHub PR feedback");
        for item in &target.pr_feedback {
            d.text(&format!("{} · {}", item.status, item.title));
        }
        d.control(
            "Inspect GitHub PR feedback".into(),
            Control::Action(Action::PrFeedback),
            false,
        );
    }
}

fn task(app: &App, id: i64, d: &mut Document) {
    let Some(task) = app.inspect_task(id) else {
        return;
    };
    d.lines.extend(tasks::task_lines(app, id, d.width, false));
    if let Some(latest) = task.latest {
        let messages = &app.current_mod().unwrap().messages;
        if let Some(index) = messages
            .iter()
            .position(|message| std::ptr::eq(message, latest))
        {
            d.heading(tasks::latest_heading(latest, task.check_origin));
            d.message(
                app,
                latest,
                &crate::message_content::messages(messages)[index]
                    .clone()
                    .task_report(latest),
            );
            d.gap();
        }
    }
    if let Some(feedback) = task.run.review_feedback.as_ref() {
        d.gap();
        d.lines.extend(findings::saved_lines(feedback, d.width));
    }
    d.gap();
    d.control(
        "View task check output".into(),
        Control::TaskChecks(id),
        false,
    );
    d.gap();
    d.control(
        "e Files, checks and worker".into(),
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
        .position(|(id, _, _)| Some(*id) == app.workspace.inspector.finding)
        .unwrap_or(0);
    for (i, (id, finding, _)) in entries.iter().enumerate() {
        let color = if matches!(finding.priority.as_str(), "P0" | "P1") {
            RED
        } else {
            AMBER
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

fn checks(app: &App, item: Option<Key>, d: &mut Document) {
    if let Some(Key::Task(id)) = item {
        if let Some(task) = app.inspect_task(id) {
            d.heading(&task.task.title);
            for activity in app
                .engine
                .check_activity()
                .into_iter()
                .filter(|activity| activity.task_id.as_deref() == Some(&task.task.id))
            {
                d.muted(&activity.label);
            }
            d.lines.extend(tasks::preparation_lines(app, id, d.width));
            d.heading(task.check_origin.label());
            if task.checks.is_empty() {
                for expected in &task.task.checks {
                    d.text(&format!("○ {expected} · not run"));
                }
            } else {
                d.lines.extend(inspector::check_lines(
                    &task.checks.iter().collect::<Vec<_>>(),
                    d.width,
                    app.show_evidence,
                ));
            }
        }
    } else {
        inspect_failure(app, d);
        d.lines.extend(inspector::check_summary(app, d.width));
    }
    d.gap();
    d.control(
        "e Commands & raw output".into(),
        Control::Evidence,
        app.show_evidence,
    );
}

fn inspect_failure(app: &App, d: &mut Document) {
    if let Some(id) = app.failed_task() {
        let number = app.inspect_task(id).unwrap().number;
        d.control(
            format!("ctrl+g f Inspect failure · task {number}"),
            Control::Action(Action::Failure),
            false,
        );
        d.gap();
    }
}

fn changes(app: &App, item: Option<Key>, d: &mut Document) {
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
    let task = match item {
        Some(Key::Task(id)) => app.inspect_task(id).map(|t| t.task),
        Some(Key::Planned(index)) => plan.tasks.get(index),
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
        d.lines.extend(rich_text::code(
            &paths
                .into_iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
            d.width,
        ));
    }
}
