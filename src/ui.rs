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
    sprout,
};

const ACCENT: Color = Color::Green;
const MUTED: Color = Color::DarkGray;
const CONTROL: Color = Color::Rgb(51, 59, 50);
const SELECTED: Color = Color::Rgb(62, 73, 55);
const KEY_HINT: Color = Color::Rgb(161, 170, 160);
const MOD_GLYPH: &str = "◇";
const DIALOG_WIDTH: u16 = 80;

pub fn input() -> TextArea<'static> {
    field("message", "Describe a feature, a fix, or an idea...")
}

pub fn name_input() -> TextArea<'static> {
    field("new code mod", "Feature or fix name...")
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
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec!["sprowt".fg(ACCENT).bold(), " harness".bold()]),
            Line::from(app.project.to_string_lossy().into_owned()).fg(MUTED),
            Line::from(
                app.current_worker()
                    .map_or(String::new(), |worker| worker.label()),
            )
            .fg(KEY_HINT),
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
        matches!(app.view, View::Mods(_)),
    );
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
        draw_conversation(frame, app, conversation);
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
    } else if let View::Queue(index) = app.view {
        draw_queue_editor(frame, app, index, dialog_area);
    } else {
        frame.render_widget(&app.input, input);
        let run = if app.current_worker().is_some_and(|worker| worker.enabled) {
            "stop"
        } else {
            "run"
        };
        let shortcuts = match app.view {
            View::NewMod if app.current_mod().is_some() => "enter create   esc back".into(),
            View::NewMod => "enter create   esc quit".into(),
            View::EditQueue(_) if footer.width >= 40 => {
                "enter save   ctrl+j newline   esc cancel".into()
            }
            View::EditQueue(_) => "↵ save  esc cancel".into(),
            _ => match footer.width {
                80.. if cfg!(target_os = "macos") => {
                    format!(
                        "↵ queue  ctrl+r {run}  ctrl+q queue  ctrl+j newline  fn+↑/↓ scroll  esc quit"
                    )
                }
                80.. => format!(
                    "↵ queue  ctrl+r {run}  ctrl+q queue  ctrl+j newline  pgup/pgdn scroll  esc quit"
                ),
                40.. => format!("↵ queue  ctrl+r {run}  ctrl+q queue  esc quit"),
                _ => format!("↵ queue  ^r {run}  ^q queue"),
            },
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
    let (list_area, new_mod) = draw_dialog(
        frame,
        area,
        &format!("code mods ({})", app.mods.len()),
        app.mods.len(),
        true,
        &[("↑↓", "select"), ("↵", "open"), ("esc", "back")],
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

fn draw_conversation(frame: &mut Frame, app: &mut App, area: Rect) {
    app.page_size = area.height.max(1);
    let [sender, body] =
        Layout::horizontal([Constraint::Length(2), Constraint::Min(1)]).areas(area);
    let mut lines = Vec::new();
    let mut labels = Vec::new();
    for message in app
        .current_mod()
        .map_or(&[][..], |code_mod| code_mod.messages.as_slice())
    {
        let text = Text::from(message.body.as_str());
        let height = Paragraph::new(text.clone())
            .wrap(Wrap { trim: false })
            .line_count(body.width);
        lines.extend(text.lines);
        labels.push(
            Line::from(if message.role == "codex" { "◆" } else { ">" })
                .fg(ACCENT)
                .bold(),
        );
        labels.resize(labels.len() + height.saturating_sub(1), Line::default());
    }
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let max_scroll = paragraph
        .line_count(body.width)
        .saturating_sub(area.height as usize);
    let max_scroll = max_scroll.min(u16::MAX as usize) as u16;
    let history_offset = app.history_offset.min(max_scroll);
    let scroll = (max_scroll - history_offset, 0);
    frame.render_widget(paragraph.scroll(scroll), body);
    frame.render_widget(Paragraph::new(labels).scroll(scroll), sender);
    app.history_offset = history_offset;
}
