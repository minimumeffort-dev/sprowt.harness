use super::*;
use crate::app::workspace_navigation::{AgentDetail, Focus};

pub(super) fn draw(app: &App, d: &mut Document) {
    let Some(agents) = app.engine.agents() else {
        return;
    };
    for (index, agent) in agents.workers.iter().enumerate() {
        if index > 0 {
            separator(d);
        }
        let title = format!(
            "{}  {}",
            agent.task_number.map_or("·".into(), |n| n.to_string()),
            agent.title
        );
        let color = match agent.tone {
            "attention" => AMBER,
            "working" => BLUE,
            "complete" => ACCENT,
            _ => MUTED,
        };
        pair(d, &title, agent.state, color, true);
        d.muted(&format!(
            "   {} · {} · w{}",
            agent.worker.provider, agent.worker.role, agent.worker.id
        ));
        d.gap();
        if let Some(failure) = &agent.failure {
            d.text(&failure.summary);
            disclosure(
                app,
                d,
                AgentDetail::Failure(agent.worker.id),
                "Failure details",
            );
            if app
                .workspace
                .agent_toggles
                .contains(&AgentDetail::Failure(agent.worker.id))
            {
                d.lines.extend(rich_text::code(&failure.detail, d.width));
            }
        } else if !agent.activity.is_empty() {
            d.muted(&agent.activity_preview);
            disclosure(
                app,
                d,
                AgentDetail::Activity(agent.worker.id),
                "Activity details",
            );
            if app
                .workspace
                .agent_toggles
                .contains(&AgentDetail::Activity(agent.worker.id))
            {
                d.text(&agent.activity);
            }
        }
        disclosure(
            app,
            d,
            AgentDetail::Worker(agent.worker.id),
            "Worker details",
        );
        if app
            .workspace
            .agent_toggles
            .contains(&AgentDetail::Worker(agent.worker.id))
        {
            d.muted(&agent.worker.identity);
            if let Some(diagnostic) = &agent.previous_diagnostic {
                d.muted("Previous worker diagnostic");
                d.lines.extend(rich_text::code(diagnostic, d.width));
            }
        }
    }
    if let Some(first) = agents.timings.first() {
        separator(d);
        disclosure(app, d, AgentDetail::Timings, "Setup timings");
        if app.workspace.agent_toggles.contains(&AgentDetail::Timings) {
            for timing in &agents.timings {
                pair(d, timing.label, &timing.duration, INK, false);
            }
            disclosure(app, d, AgentDetail::TimingSource, "Exact timing evidence");
            if app
                .workspace
                .agent_toggles
                .contains(&AgentDetail::TimingSource)
            {
                d.lines.extend(rich_text::code(
                    &app.current_mod().unwrap().diagnostics.timing_packet(None),
                    d.width,
                ));
            }
        } else {
            pair(d, first.label, &first.duration, MUTED, false);
        }
    }
}
fn separator(d: &mut Document) {
    d.gap();
    d.lines
        .push(Line::from("─".repeat(usize::from(d.width))).fg(BORDER));
    d.gap();
}
fn disclosure(app: &App, d: &mut Document, detail: AgentDetail, label: &str) {
    let open = app.workspace.agent_toggles.contains(&detail);
    let selected =
        app.workspace.agent_cursor == Some(detail) && app.workspace.focus == Focus::Content;
    d.control(
        format!("{} {label}", if open { "▾" } else { "▸" }),
        Control::AgentDetail(detail),
        selected,
    );
    if !selected {
        d.lines.last_mut().unwrap().style.bg = None;
    }
}
fn pair(d: &mut Document, left: &str, right: &str, color: Color, bold: bool) {
    let left_width = Line::from(left).width();
    let right_width = Line::from(right).width();
    if left_width + right_width + 3 <= usize::from(d.width) {
        let mut line = Line::from(vec![
            Span::raw(left.to_owned()),
            Span::raw(" ".repeat(usize::from(d.width) - left_width - right_width)),
            right.to_owned().fg(color),
        ]);
        if bold {
            line = line.bold();
        }
        d.lines.push(line);
    } else {
        d.lines.extend(wrap_line(
            if bold {
                Line::from(left.to_owned()).bold()
            } else {
                Line::from(left.to_owned())
            },
            d.width,
            0,
        ));
        d.lines.push(Line::from(right.to_owned()).fg(color));
    }
}
