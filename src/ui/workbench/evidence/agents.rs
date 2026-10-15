use super::*;
use crate::app::workspace_navigation::{AgentDetail, Focus};
use crate::engine::agents::{Agent, Timing};

pub(super) fn draw(app: &App, d: &mut Document) {
    let Some(agents) = app.engine.agents() else {
        return;
    };
    d.width = d.width.min(96);
    for (index, agent) in agents.workers.iter().enumerate() {
        if index > 0 {
            d.gap();
        }
        let mut card = Document::new(d.width.saturating_sub(4));
        let color = match agent.tone {
            "attention" => AMBER,
            "working" => BLUE,
            "complete" => ACCENT,
            _ => MUTED,
        };
        pair(
            &mut card,
            &agent
                .task_number
                .map_or_else(|| agent.worker.role.to_owned(), |n| format!("Task {n}")),
            agent.state,
            color,
        );
        card.gap();
        card.lines.extend(wrap_line(
            Line::from(agent.title.clone()).bold(),
            card.width,
            0,
        ));
        let activity = agent
            .failure
            .as_ref()
            .map_or(agent.activity_preview.as_str(), |failure| &failure.summary);
        if !activity.is_empty() {
            card.gap();
            card.muted(&format!(
                "{} {activity}",
                if agent.failure.is_some() { "!" } else { "◌" }
            ));
        }
        rule(&mut card);
        ribbon(app, &mut card, agent);
        if !agent.output.is_empty()
            && !app
                .workspace
                .agent_toggles
                .contains(&AgentDetail::ActivityHidden(agent.worker.id))
        {
            rule(&mut card);
            output(&mut card, &agent.output);
        }
        panel(d, card);
    }
    if !agents.timings.is_empty() {
        d.gap();
        d.gap();
        let all = app
            .workspace
            .agent_toggles
            .contains(&AgentDetail::AllTimings);
        let label = if all {
            "Show longest 3".into()
        } else {
            format!("Show all {}", agents.timings.len())
        };
        if agents.timings.len() > 3 {
            toggle(app, d, AgentDetail::AllTimings, "Setup timings", &label);
        } else {
            d.lines.push(Line::from("Setup timings").bold());
        }
        d.gap();
        for timing in agents.timings.iter().take(if all { usize::MAX } else { 3 }) {
            timing_row(d, timing);
        }
        d.gap();
        d.muted("Recorded operation durations · may overlap");
    }
}

fn ribbon(app: &App, d: &mut Document, agent: &Agent) {
    let has_output = !agent.output.is_empty();
    let inline = has_output && d.width >= 68;
    let fields_width = if inline { d.width - 19 } else { d.width };
    let column = usize::from(fields_width) / 3;
    let fields = [
        ("Worker", agent.worker_label.as_str()),
        ("Model", agent.model.as_str()),
        ("Reasoning", agent.reasoning.as_str()),
    ];
    let mut labels = Vec::new();
    for (index, (label, _)) in fields.iter().enumerate() {
        if index > 0 {
            labels.push(" │ ".fg(BORDER));
        }
        labels.push(format!("{label:<width$}", width = column.saturating_sub(3)).fg(MUTED));
    }
    d.lines.push(Line::from(labels).bg(CONTROL));
    let key = AgentDetail::ActivityHidden(agent.worker.id);
    let label = if app.workspace.agent_toggles.contains(&key) {
        "Show activity"
    } else {
        "Hide activity"
    };
    if inline {
        let row = d.lines.len() - 1;
        let left = d.lines[row].width();
        d.lines[row].spans.push(Span::raw(
            " ".repeat(usize::from(d.width).saturating_sub(left + label.len() + 2)),
        ));
        d.lines[row].spans.push(control_span(app, key, label));
        d.controls.push((row, Control::AgentDetail(key)));
    }
    let columns: Vec<_> = fields
        .iter()
        .map(|(_, value)| {
            wrap_line(
                Line::from((*value).to_owned()),
                column.saturating_sub(3) as u16,
                0,
            )
        })
        .collect();
    for row in 0..columns.iter().map(Vec::len).max().unwrap_or(0) {
        let mut spans = Vec::new();
        for (index, lines) in columns.iter().enumerate() {
            if index > 0 {
                spans.push(" │ ".fg(BORDER));
            }
            let text = lines.get(row).map_or(String::new(), Line::to_string);
            let padding = column
                .saturating_sub(3)
                .saturating_sub(Line::from(text.clone()).width());
            spans.push(Span::raw(format!("{text}{}", " ".repeat(padding))));
        }
        d.lines.push(Line::from(spans).bg(CONTROL));
    }
    if has_output && !inline {
        toggle(app, d, key, "", label);
    }
}

fn control_span(app: &App, key: AgentDetail, label: &str) -> Span<'static> {
    let selected = app.workspace.agent_cursor == Some(key) && app.workspace.focus == Focus::Content;
    let span = format!("{label}{}", if selected { " ↵" } else { "  " }).fg(ACCENT);
    if selected { span.bg(SELECTED) } else { span }
}

fn toggle(app: &App, d: &mut Document, key: AgentDetail, title: &str, label: &str) {
    d.controls.push((d.lines.len(), Control::AgentDetail(key)));
    d.lines.push(Line::from(vec![
        title.to_owned().bold(),
        Span::raw(" ".repeat(usize::from(d.width).saturating_sub(title.len() + label.len() + 2))),
        control_span(app, key, label),
    ]));
}

fn output(d: &mut Document, source: &str) {
    let digits = source.split('\n').count().to_string().len().max(2);
    let width = d.width.saturating_sub(digits as u16 + 2);
    for (index, line) in source.split('\n').enumerate() {
        let rows = rich_text::code(line, width + 2);
        for (part, mut row) in rows.into_iter().enumerate() {
            row.spans.remove(0);
            row.style = Style::new().fg(INK);
            row.spans.insert(
                0,
                if part == 0 {
                    format!("{:0digits$}  ", index + 1).fg(MUTED)
                } else {
                    " ".repeat(digits + 2).fg(MUTED)
                },
            );
            d.lines.push(row);
        }
    }
}

fn panel(d: &mut Document, card: Document) {
    let edge = "─".repeat(usize::from(d.width.saturating_sub(2)));
    d.lines.push(Line::from(format!("┌{edge}┐")).fg(BORDER));
    let start = d.lines.len();
    for line in card.lines {
        let padding = usize::from(card.width).saturating_sub(line.width());
        let style = line.style;
        let mut spans = vec!["│ ".fg(BORDER)];
        spans.extend(line.spans.into_iter().map(|mut span| {
            span.style = style.patch(span.style);
            span
        }));
        spans.push(Span::styled(" ".repeat(padding), style));
        spans.push(" │".fg(BORDER));
        d.lines.push(Line::from(spans));
    }
    d.controls.extend(
        card.controls
            .into_iter()
            .map(|(row, control)| (start + row, control)),
    );
    d.lines.push(Line::from(format!("└{edge}┘")).fg(BORDER));
}

fn rule(d: &mut Document) {
    d.lines
        .push(Line::from("─".repeat(usize::from(d.width))).fg(BORDER));
}

fn timing_row(d: &mut Document, timing: &Timing) {
    if d.width < 60 {
        pair(d, timing.label, &timing.duration, INK);
        let width = usize::from(d.width);
        d.lines.push(
            Line::from("━".repeat((timing.relative_duration * width as f64).round() as usize))
                .fg(ACCENT),
        );
    } else {
        let label_width = 28;
        let bar_width = usize::from(d.width).saturating_sub(label_width + 11);
        let filled = (timing.relative_duration * bar_width as f64).round() as usize;
        d.lines.push(Line::from(vec![
            Span::raw(format!("{:<label_width$}", timing.label)),
            "━".repeat(filled).fg(ACCENT),
            "─".repeat(bar_width - filled).fg(BORDER),
            format!("{:>11}", timing.duration).bold(),
        ]));
    }
}

fn pair(d: &mut Document, left: &str, right: &str, color: Color) {
    let left_width = Line::from(left).width();
    let right_width = Line::from(right).width();
    if left_width + right_width + 2 <= usize::from(d.width) {
        d.lines.push(Line::from(vec![
            Span::raw(left.to_owned()),
            Span::raw(" ".repeat(usize::from(d.width) - left_width - right_width)),
            right.to_owned().fg(color),
        ]));
    } else {
        d.lines
            .extend(wrap_line(Line::from(left.to_owned()), d.width, 0));
        d.lines.push(Line::from(right.to_owned()).fg(color));
    }
}
