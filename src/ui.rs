use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, List, ListState, Padding, Paragraph, Wrap},
};
use ratatui_textarea::{TextArea, WrapMode};
use tachyonfx::{Effect, Interpolation, fx};

use crate::{
    app::{App, View},
    plan::{Plan, Planning},
    sprout,
    store::CodeMod,
    worker::Status,
};

const ACCENT: Color = Color::Green;
const MUTED: Color = Color::DarkGray;
const CONTROL: Color = Color::Rgb(51, 59, 50);
const SELECTED: Color = Color::Rgb(62, 73, 55);
const USER_BACKGROUND: Color = Color::Rgb(43, 49, 43);
const KEY_HINT: Color = Color::Rgb(161, 170, 160);
const MOD_GLYPH: &str = "◇";
const DIALOG_WIDTH: u16 = 80;

pub fn input() -> TextArea<'static> {
    field("message", "Describe a feature, a fix, or an idea...")
}

pub fn name_input() -> TextArea<'static> {
    field(
        "new code mod",
        "Describe what you want to build or change...",
    )
}

pub fn edit_input() -> TextArea<'static> {
    field("edit queued message", "")
}

fn field(title: &'static str, placeholder: &'static str) -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(ACCENT))
            .padding(Padding::horizontal(1))
            .title(format!(" {title} ")),
    );
    input.set_cursor_line_style(Style::default());
    input.set_placeholder_text(placeholder);
    input.set_placeholder_style(Style::new().fg(MUTED));
    input.set_wrap_mode(WrapMode::WordOrGlyph);
    input
}

pub fn welcome_effect(motion: bool) -> Effect {
    fx::fade_from_fg(
        MUTED,
        (if motion { 400 } else { 0 }, Interpolation::SineOut),
    )
}

pub fn draw(frame: &mut Frame, app: &mut App, pose: sprout::Pose) -> Rect {
    let area = frame.area().inner(Margin::new(2, 1));
    let mod_height = u16::from(!matches!(app.view, View::NewMod));
    if area.width < 32 || area.height < 11 + mod_height {
        frame.render_widget(
            Paragraph::new("Make the terminal a little larger.\nCtrl+C to quit.")
                .wrap(Wrap { trim: false }),
            area,
        );
        return Rect::default();
    }

    let gap = u16::from(area.height >= 13 + 2 * mod_height);
    let [header, _, mod_row, _, content, _, input, footer] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(gap),
        Constraint::Length(mod_height),
        Constraint::Length(gap * mod_height),
        Constraint::Min(1),
        Constraint::Length(gap),
        Constraint::Length(5),
        Constraint::Length(1),
    ])
    .areas(area);

    let [companion, heading] =
        Layout::horizontal([Constraint::Length(10), Constraint::Min(1)]).areas(header);
    sprout::draw(frame, companion, pose);
    let project = directories::BaseDirs::new()
        .and_then(|dirs| {
            app.project
                .strip_prefix(dirs.home_dir())
                .ok()
                .map(|path| format!("~/{}", path.display()))
        })
        .unwrap_or_else(|| app.project.display().to_string());
    let status = app.current_worker().map_or_else(
        || {
            app.current_mod()
                .and_then(|code_mod| code_mod.planning.as_ref())
                .filter(|planning| planning.status != "ready")
                .map_or(String::new(), |_| "▤ planning paused".into())
        },
        |worker| {
            if worker.status == Status::Complete {
                String::new()
            } else {
                worker.label()
            }
        },
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec!["sprowt".fg(ACCENT).bold(), " harness".bold()]),
            Line::from(fit_name(&project, heading.width)).fg(MUTED),
            Line::from(fit_name(&status, heading.width)).fg(KEY_HINT),
            Line::from(
                app.worker_error()
                    .map_or(String::new(), |error| fit_name(error, heading.width)),
            )
            .fg(Color::Red),
        ]),
        heading,
    );

    draw_mod_selector(
        frame,
        app.current_mod()
            .map_or("", |code_mod| code_mod.name.as_str()),
        mod_row,
        matches!(app.view, View::Mods(_) | View::DeleteMod(_)),
    );
    let mut can_scroll = false;
    if matches!(app.view, View::Chat | View::EditQueue(_)) {
        let count = app.current_mod().map_or(0, |code_mod| code_mod.queue.len());
        let steering = app
            .current_mod()
            .map_or(0, |code_mod| code_mod.steering.len());
        let steering_height = if steering == 0 {
            0
        } else {
            (steering.min(3) as u16 + 1).min(content.height)
        };
        let pending_gap =
            u16::from(count > 0 && steering_height > 0 && content.height > steering_height);
        let queue_height = if count == 0 {
            0
        } else {
            (count.min(3) as u16 + 1)
                .min(content.height.saturating_sub(steering_height + pending_gap))
        };
        let pending_height = queue_height + pending_gap + steering_height;
        let separation = u16::from(pending_height > 0 && content.height > pending_height);
        let [conversation, _, queue, _, steering] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(separation),
            Constraint::Length(queue_height),
            Constraint::Length(pending_gap),
            Constraint::Length(steering_height),
        ])
        .areas(content);
        can_scroll = draw_conversation(frame, app, conversation);
        draw_queue_preview(frame, app, queue);
        draw_steering_preview(frame, app, steering);
    }
    let dialog_area = Rect {
        y: mod_row.bottom() + gap,
        width: area.width.min(DIALOG_WIDTH),
        height: area.bottom().saturating_sub(mod_row.bottom() + gap),
        ..area
    };
    if let View::Mods(index) = app.view {
        draw_mod_picker(frame, app, index, dialog_area);
    } else if let View::DeleteMod(index) = app.view {
        draw_delete_mod(frame, app, index, dialog_area);
    } else if let View::Queue(index) = app.view {
        draw_queue_editor(frame, app, index, dialog_area);
    } else {
        frame.render_widget(&app.input, input);
        let shortcuts = match app.view {
            View::NewMod => format!(
                "↵ create + plan{}   esc {}",
                if footer.width >= 48 {
                    "   ctrl+j newline"
                } else {
                    ""
                },
                if app.current_mod().is_some() {
                    "back"
                } else {
                    "quit"
                }
            ),
            View::EditQueue(_) if footer.width >= 40 => {
                "enter save   ctrl+j newline   esc cancel".into()
            }
            View::EditQueue(_) => "↵ save  esc cancel".into(),
            _ => chat_hints(app, footer.width, can_scroll),
        };
        frame.render_widget(Line::from(shortcuts).fg(MUTED), footer);
    }
    heading
}

fn queue_items(app: &App, width: u16) -> impl Iterator<Item = Line<'static>> + '_ {
    app.current_mod()
        .map_or(&[][..], |code_mod| code_mod.queue.as_slice())
        .iter()
        .map(move |message| {
            let marker = if matches!(app.view, View::Queue(_)) {
                if app.queue_selection.contains(&message.id) {
                    "✓ "
                } else {
                    "· "
                }
            } else {
                "> "
            };
            Line::from(vec![
                marker.fg(ACCENT).bold(),
                fit_name(&message.body.replace('\n', " "), width.saturating_sub(2)).into(),
            ])
        })
}

fn draw_steering_preview(frame: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let messages = app
        .current_mod()
        .map_or(&[][..], |code_mod| code_mod.steering.as_slice());
    let [title, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let waiting = if app.current_worker().is_some() {
        "waiting for active turn"
    } else {
        "waiting for workers"
    };
    frame.render_widget(
        Line::from(fit_name(
            &format!("steering ({}) · {waiting}", messages.len()),
            title.width,
        ))
        .fg(KEY_HINT),
        title,
    );
    frame.render_widget(
        List::new(messages.iter().map(|message| {
            Line::from(vec![
                "↳ ".fg(ACCENT),
                fit_name(&message.replace('\n', " "), body.width.saturating_sub(2)).into(),
            ])
        })),
        body,
    );
}

fn draw_queue_preview(frame: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let count = app.current_mod().map_or(0, |code_mod| code_mod.queue.len());
    let [title, messages] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    frame.render_widget(Line::from(format!("queue ({count})")).fg(KEY_HINT), title);
    let selection = if let View::EditQueue(index) = app.view {
        Some(index)
    } else {
        None
    };
    frame.render_stateful_widget(
        List::new(queue_items(app, messages.width)).highlight_style(Style::new().bg(CONTROL)),
        messages,
        &mut ListState::default().with_selected(selection),
    );
}

fn draw_queue_editor(frame: &mut Frame, app: &App, index: usize, area: Rect) {
    let count = app.current_mod().map_or(0, |code_mod| code_mod.queue.len());
    let mut hints = vec![
        ("↑↓", "focus"),
        ("space", "mark"),
        ("s", "steer"),
        ("↵", "edit"),
        ("d", "remove"),
    ];
    if area.width >= 50 {
        hints.extend([("k", "move↑"), ("j", "move↓")]);
    } else {
        hints.push(("k/j", "reorder"));
    }
    hints.push(("esc", "back"));
    let title = if app.queue_selection.is_empty() {
        format!("queue ({count})")
    } else {
        format!("queue ({count}) · {} selected", app.queue_selection.len())
    };
    let (messages, _) = draw_dialog(frame, area, &title, count, false, &hints);
    frame.render_stateful_widget(
        List::new(queue_items(app, messages.width)).highlight_style(dialog_selection()),
        messages,
        &mut ListState::default().with_selected((count > 0).then_some(index)),
    );
}

fn draw_mod_selector(frame: &mut Frame, name: &str, area: Rect, open: bool) {
    let [label, selector, _] = Layout::horizontal([
        Constraint::Length(12),
        Constraint::Length(area.width.saturating_sub(12).min(54)),
        Constraint::Min(0),
    ])
    .areas(area);
    frame.render_widget(Line::from("<code mod/>").fg(KEY_HINT), label);
    frame.render_widget(
        Block::new()
            .fg(Color::White)
            .bg(if open { SELECTED } else { CONTROL }),
        selector,
    );
    let [_, icon, title, shortcut] = Layout::horizontal([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(11),
    ])
    .areas(selector);
    frame.render_widget(Line::from(MOD_GLYPH).fg(ACCENT), icon);
    frame.render_widget(Line::from(fit_name(name, title.width)), title);
    frame.render_widget(
        Line::from(vec![
            format!(" {}  ", if open { "▴" } else { "▾" }).fg(ACCENT),
            "ctrl+p ".fg(KEY_HINT),
        ]),
        shortcut,
    );
}

fn draw_mod_picker(frame: &mut Frame, app: &App, index: usize, area: Rect) {
    let mut hints = vec![("↑↓", "select"), ("↵", "open")];
    if index < app.mods.len() {
        hints.push(("d", "delete"));
    }
    hints.push(("esc", "back"));
    let (list_area, new_mod) = draw_dialog(
        frame,
        area,
        &format!("code mods ({})", app.mods.len()),
        app.mods.len(),
        true,
        &hints,
    );
    let items = app.mods.iter().map(|code_mod| {
        let active = app
            .current_mod()
            .is_some_and(|active| active.id == code_mod.id);
        Line::from(vec![
            format!("{MOD_GLYPH} ").fg(ACCENT),
            Span::raw(fit_name(&code_mod.name, list_area.width.saturating_sub(4))),
            (if active { " ✓" } else { "" }).fg(ACCENT),
        ])
    });
    let selected = dialog_selection();
    let highlight = if index < app.mods.len() {
        selected
    } else {
        Style::new()
    };
    frame.render_stateful_widget(
        List::new(items).highlight_style(highlight),
        list_area,
        &mut ListState::default().with_selected(Some(index.min(app.mods.len().saturating_sub(1)))),
    );
    let action = Line::from(vec!["+ ".fg(ACCENT), "new code mod".into()]);
    frame.render_widget(
        if index == app.mods.len() {
            action.style(selected)
        } else {
            action
        },
        new_mod,
    );
}

fn draw_delete_mod(frame: &mut Frame, app: &App, index: usize, area: Rect) {
    let code_mod = &app.mods[index];
    let removal = if app.has_worker(code_mod.id) {
        "Stops worker; removes history, queue and draft."
    } else {
        "Removes saved history, queue and draft."
    };
    let lines = vec![
        Line::from(vec![
            format!("{MOD_GLYPH} ").fg(ACCENT),
            fit_name(&code_mod.name, area.width.saturating_sub(6)).bold(),
        ]),
        Line::from(removal).fg(KEY_HINT),
    ];
    let body = Paragraph::new(lines).wrap(Wrap { trim: false });
    let rows = body.line_count(area.width.saturating_sub(4));
    let (content, _) = draw_dialog(
        frame,
        area,
        "delete code mod?",
        rows,
        false,
        &[("↵", "delete"), ("esc", "cancel")],
    );
    frame.render_widget(body, content);
}

fn dialog_selection() -> Style {
    Style::new().fg(Color::White).bg(SELECTED)
}

fn draw_dialog(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    rows: usize,
    has_action: bool,
    shortcuts: &[(&str, &str)],
) -> (Rect, Rect) {
    let mut hints = Vec::new();
    for (index, (key, label)) in shortcuts.iter().enumerate() {
        if index > 0 {
            hints.push(Span::raw("  "));
        }
        // Keep each key and its description together when wrapping.
        hints.push(format!("{key}\u{a0}").fg(Color::White).bold());
        hints.push(label.replace(' ', "\u{a0}").fg(KEY_HINT));
    }
    let hints = Paragraph::new(Line::from(hints)).wrap(Wrap { trim: false });
    let hints_height = hints.line_count(area.width.saturating_sub(4)) as u16;
    let action_height = u16::from(has_action);
    let panel = Rect {
        height: (rows.clamp(1, 7) as u16 + action_height + 3 + hints_height).min(area.height),
        ..area
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(MUTED))
        .title(Line::from(format!(" {title} ")).fg(KEY_HINT).bold());
    let inner = block.inner(panel).inner(Margin::new(1, 0));
    frame.render_widget(block, panel);
    let [list, action, divider, footer] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(action_height),
        Constraint::Length(1),
        Constraint::Length(hints_height),
    ])
    .areas(inner);
    frame.render_widget(
        Line::from(format!(
            "├{}┤",
            "─".repeat(panel.width.saturating_sub(2) as usize)
        ))
        .fg(MUTED),
        Rect {
            x: panel.x,
            width: panel.width,
            ..divider
        },
    );
    frame.render_widget(hints, footer);
    (list, action)
}

fn fit_name(name: &str, width: u16) -> String {
    if width == 0 {
        return String::new();
    }
    let span = Span::raw(name);
    if span.width() <= usize::from(width) {
        return name.to_owned();
    }
    let mut remaining = usize::from(width - 1);
    let mut title = String::new();
    for grapheme in span.styled_graphemes(Style::new()) {
        let width = Span::raw(grapheme.symbol).width();
        if width > remaining {
            break;
        }
        title.push_str(grapheme.symbol);
        remaining -= width;
    }
    title.push('…');
    title
}

fn chat_hints(app: &App, width: u16, can_scroll: bool) -> String {
    let queued = app.current_mod().is_some_and(|m| !m.queue.is_empty());
    let worker = app.current_worker();
    let run = if worker.is_some_and(|w| w.enabled) {
        Some("stop")
    } else if app
        .current_mod()
        .and_then(|m| m.planning.as_ref())
        .is_some_and(|p| p.status != "ready")
    {
        Some("retry")
    } else if queued || worker.is_some_and(|w| w.status != Status::Complete) {
        Some("run")
    } else {
        None
    };
    let ctrl = if width < 60 { "^" } else { "ctrl+" };
    let mut options = Vec::new();
    if let Some(action) = run {
        options.push(format!("{ctrl}r {action}"));
    }
    if queued {
        options.push(format!("{ctrl}q manage"));
    }
    options.push(format!("{ctrl}j newline"));
    if can_scroll {
        options.push(if cfg!(target_os = "macos") {
            "fn+↑/↓ scroll".into()
        } else {
            "pgup/pgdn scroll".into()
        });
    }
    let mut hints = "↵ queue".to_owned();
    for option in options {
        let candidate = format!("{hints}  {option}");
        if Span::raw(&candidate).width() + "  esc quit".len() <= width as usize {
            hints = candidate;
        }
    }
    format!("{hints}  esc quit")
}

fn plan_lines(plan: &Plan, planning: &Planning, details: bool) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from("▤ codex · planner").fg(ACCENT).bold(),
        Line::from(format!(
            "plan ready · {} {}",
            plan.tasks.len(),
            if plan.tasks.len() == 1 {
                "task"
            } else {
                "tasks"
            }
        ))
        .fg(ACCENT)
        .bold(),
    ];
    for (index, task) in plan.tasks.iter().enumerate() {
        lines.push(Line::default());
        lines.push(Line::from(vec![
            format!("{}. ", index + 1).fg(ACCENT).bold(),
            task.title.clone().bold(),
        ]));
        lines.push(Line::from(format!("   {}", task.outcome)));
        let dependencies: Vec<_> = task
            .depends_on
            .iter()
            .filter_map(|id| plan.tasks.iter().position(|task| &task.id == id))
            .map(|index| (index + 1).to_string())
            .collect();
        if !dependencies.is_empty() {
            lines.push(
                Line::from(format!(
                    "   after {} {}",
                    if dependencies.len() == 1 {
                        "task"
                    } else {
                        "tasks"
                    },
                    dependencies.join(", ")
                ))
                .fg(KEY_HINT),
            );
        }
        if details {
            if !task.files.is_empty() {
                lines
                    .push(Line::from(format!("   files · {}", task.files.join(", "))).fg(KEY_HINT));
            }
            lines.push(Line::from("   checks").fg(KEY_HINT));
            lines.extend(
                task.checks
                    .iter()
                    .map(|check| Line::from(format!("   · {check}"))),
            );
        }
    }
    lines.push(Line::default());
    if details {
        if let Some(model) = &planning.model {
            lines.push(
                Line::from(format!(
                    "model · {model} · {}",
                    planning.effort.as_deref().unwrap_or("medium")
                ))
                .fg(KEY_HINT),
            );
        }
        if let Some(routing) = &planning.routing {
            lines.push(Line::from(routing.clone()).fg(MUTED));
        }
        lines.push(Line::default());
    }
    lines.push(Line::from(vec![
        if details {
            "▾ hide details"
        } else {
            "▸ files, checks & model"
        }
        .fg(KEY_HINT),
        "  ctrl+o".fg(ACCENT),
    ]));
    lines.push(Line::default());
    lines.push(Line::from("read-only · code changes come next").fg(KEY_HINT));
    lines
}

struct ConversationBlock<'a> {
    text: Text<'a>,
    user: bool,
    plan: bool,
}

fn conversation_blocks(code_mod: &CodeMod, details: bool) -> Vec<ConversationBlock<'_>> {
    let saved = code_mod
        .planning
        .as_ref()
        .filter(|p| p.status == "ready")
        .and_then(|p| p.plan.as_ref().map(|plan| (p, plan)));
    let plan_id = saved.map(|(p, _)| format!("plan:{}", p.source));
    code_mod
        .messages
        .iter()
        .filter_map(|message| {
            let is_plan = plan_id.is_some() && message.item_id == plan_id;
            if let Some((planning, plan)) = saved
                && is_plan
            {
                return Some(ConversationBlock {
                    text: plan_lines(plan, planning, details).into(),
                    user: false,
                    plan: true,
                });
            }
            if saved.is_some() && message.role == "planner" {
                return None;
            }
            let user = message.role == "user";
            let mut text = Text::from(message.body.as_str());
            if !user {
                text.lines.insert(
                    0,
                    Line::from(if message.role == "planner" {
                        "▤ codex · planner"
                    } else {
                        "◆ codex · executor"
                    })
                    .fg(ACCENT)
                    .bold(),
                );
            }
            Some(ConversationBlock {
                text,
                user,
                plan: false,
            })
        })
        .collect()
}

fn draw_conversation(frame: &mut Frame, app: &mut App, area: Rect) -> bool {
    app.page_size = area.height.max(1);
    if area.is_empty() {
        return false;
    }
    let blocks = app
        .current_mod()
        .map_or_else(Vec::new, |m| conversation_blocks(m, app.plan_details));
    let mut rows = Vec::new();
    let mut total = 0;
    let mut plan_top = 0;
    for block in blocks {
        let width = area.width.saturating_sub(if block.user { 2 } else { 0 });
        let paragraph = Paragraph::new(block.text).wrap(Wrap { trim: false });
        let height = paragraph.line_count(width);
        if block.plan {
            plan_top = total;
        }
        rows.push((total, height, block.user, paragraph));
        total += height + 1;
    }
    let max_scroll = total.saturating_sub(1).saturating_sub(area.height as usize);
    let history_offset = if app.focus_plan {
        max_scroll.saturating_sub(plan_top).min(u16::MAX as usize) as u16
    } else {
        app.history_offset
            .min(max_scroll.min(u16::MAX as usize) as u16)
    };
    let scroll = max_scroll - history_offset as usize;
    for (top, height, user, paragraph) in rows {
        let visible_start = top.max(scroll);
        let visible_end = (top + height).min(scroll + area.height as usize);
        if visible_start >= visible_end {
            continue;
        }
        let visible = Rect {
            y: area.y + (visible_start - scroll) as u16,
            height: (visible_end - visible_start) as u16,
            ..area
        };
        if user {
            frame.render_widget(Block::new().bg(USER_BACKGROUND).fg(Color::White), visible);
            if visible_start == top {
                frame.render_widget(
                    Line::from(">").fg(ACCENT).bold(),
                    Rect {
                        width: 1,
                        height: 1,
                        ..visible
                    },
                );
            }
        }
        let body = Rect {
            x: visible.x + if user { 2 } else { 0 },
            width: visible.width.saturating_sub(if user { 2 } else { 0 }),
            ..visible
        };
        frame.render_widget(paragraph.scroll(((visible_start - top) as u16, 0)), body);
    }
    app.history_offset = history_offset;
    app.focus_plan = false;
    max_scroll > 0
}
