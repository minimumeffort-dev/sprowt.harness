use super::*;
use ratatui::widgets::ListItem;

pub(super) fn panel(frame: &mut Frame, area: Rect, title: String, keys: &[(&str, &str)]) -> Rect {
    let hints = key_hints(keys);
    let [panel, footer] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(hints.line_count(area.width) as u16),
    ])
    .areas(area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER))
        .padding(Padding::horizontal(1))
        .title(Line::from(format!(" {title} ")).fg(KEY_HINT).bold());
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    frame.render_widget(hints, footer);
    inner
}

fn state_style(state: &str) -> Style {
    Style::new().fg(
        if matches!(
            state,
            "Check failed" | "Final checks failed" | "Worker stopped" | "Blocked"
        ) {
            Color::Red
        } else if matches!(state, "Done" | "Working" | "Checking" | "Starting") {
            ACCENT
        } else {
            KEY_HINT
        },
    )
}

pub(super) fn draw_tasks(frame: &mut Frame, app: &mut App, selected: usize, area: Rect) {
    let ids = app.task_ids();
    let selected = selected.min(ids.len().saturating_sub(1));
    let inner = panel(
        frame,
        area,
        format!("tasks ({})", ids.len()),
        &[
            ("↑↓", "select"),
            ("↵", "inspect"),
            ("h", "all history"),
            ("esc", "back"),
        ],
    );
    let items: Vec<_> = ids
        .iter()
        .filter_map(|id| app.inspect_task(*id))
        .map(|task| {
            let mut lines = wrap_line(
                Line::from(format!("{}. {}", task.number, task.task.title)).bold(),
                inner.width,
                3,
            );
            lines.extend(wrap_line(
                Line::from(vec![
                    Span::raw(format!("   {}", task.state)).style(state_style(task.state)),
                    format!(" · {}", task.identity).fg(KEY_HINT),
                ]),
                inner.width,
                3,
            ));
            lines.push(Line::default());
            ListItem::new(lines)
        })
        .collect();
    if items.is_empty() {
        frame.render_widget(Paragraph::new("No tasks in the current plan."), inner);
    } else {
        let mut state = ListState::default().with_selected(Some(selected));
        frame.render_stateful_widget(
            List::new(items).highlight_style(dialog_selection()),
            inner,
            &mut state,
        );
    }
    app.scroll.content = inner;
    app.page_size = (inner.height / 3).max(1);
    app.view = View::Tasks(selected);
}

pub(super) fn draw_task(frame: &mut Frame, app: &mut App, id: i64, scroll: u16, area: Rect) {
    let Some(task) = app.inspect_task(id) else {
        app.view = View::Tasks(0);
        draw_tasks(frame, app, 0, area);
        return;
    };
    let mut keys = vec![("↑↓", "scroll"), ("h", "task history")];
    let actions = app.action_dock();
    for action in &actions.actions {
        match action.action {
            Action::Run | Action::Retry => keys.push(("ctrl+r", action.label.as_str())),
            Action::RetryGit => keys.push(("ctrl+r", action.label.as_str())),
            Action::Network => keys.push(("ctrl+n", "review domains")),
            _ => {}
        }
    }
    keys.extend([("esc", "tasks"), ("ctrl+t", "conversation")]);
    let inner = panel(frame, area, format!("task {}", task.number), &keys);
    let mut lines = vec![
        Line::from(task.task.title.clone()).bold(),
        Line::from(task.identity).fg(KEY_HINT),
        Line::from(task.state).style(state_style(task.state)),
        Line::default(),
    ];
    if !task.note.is_empty() {
        lines.extend(
            Text::from(task.note.as_str())
                .lines
                .into_iter()
                .map(|line| Line::from(line.to_string())),
        );
        lines.push(Line::default());
    }
    if let Some(error) = task.error.filter(|error| *error != task.note) {
        lines.extend(
            error
                .lines()
                .map(|line| Line::from(line.to_owned()).fg(Color::Red)),
        );
        lines.push(Line::default());
    }
    if let Some(check) = task.checks.iter().find(|check| check.failed()) {
        lines.push(
            Line::from(format!("! {}", check.check))
                .fg(Color::Red)
                .bold(),
        );
        lines.extend(command_lines(&check.command, inner.width));
        lines.extend(
            check
                .evidence()
                .lines()
                .map(|line| Line::from(line.to_owned()).fg(Color::Red)),
        );
        lines.push(Line::default());
    }
    if let Some(latest) = task.latest {
        lines.push(Line::from("Latest update").fg(KEY_HINT).bold());
        let mut body = latest.body.chars().take(600).collect::<String>();
        if body.len() < latest.body.len() {
            body.push('…');
        }
        lines.extend(Text::from(body).lines);
        lines.push(Line::default());
    } else if task.run.status == "done" && !task.run.summary.is_empty() {
        lines.push(Line::from("Worker report").fg(KEY_HINT).bold());
        lines.extend(Text::from(task.run.summary.clone()).lines);
        lines.push(Line::default());
    }
    let passed = task
        .checks
        .iter()
        .filter(|check| check.exit_code == Some(0))
        .count();
    let failed = task.checks.iter().filter(|check| check.failed()).count();
    let unrun = task.checks.iter().filter(|check| check.skipped()).count();
    lines.push(
        Line::from(if task.checks.is_empty() {
            "Checks".into()
        } else {
            format!(
                "{} · {passed} passed · {failed} failed · {unrun} not run",
                task.checks_label
            )
        })
        .fg(KEY_HINT)
        .bold(),
    );
    for expected in &task.task.checks {
        let checks: Vec<_> = task
            .checks
            .iter()
            .filter(|check| check.check == *expected)
            .collect();
        let marker = if checks.is_empty() || checks.iter().all(|check| check.skipped()) {
            "·"
        } else if checks.iter().any(|check| check.failed()) {
            "!"
        } else if checks.iter().all(|check| check.exit_code == Some(0)) {
            "✓"
        } else {
            "·"
        };
        lines.push(
            Line::from(format!("{marker} {expected}")).fg(if marker == "!" {
                Color::Red
            } else {
                KEY_HINT
            }),
        );
        for check in checks.into_iter().filter(|check| check.failed()) {
            if !task
                .checks
                .iter()
                .find(|c| c.failed())
                .is_some_and(|first| std::ptr::eq(*first, *check))
            {
                lines.extend(command_lines(&check.command, inner.width));
                lines.extend(
                    check
                        .evidence()
                        .lines()
                        .map(|line| Line::from(line.to_owned()).fg(Color::Red)),
                );
            }
        }
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
    app.view = View::Task(id, scroll);
}
