mod findings;
mod inspector;
mod tasks;

use std::time::Duration;

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
    app::{Action, ActionDock, ActionItem, App, Tone, View},
    execution::Execution,
    plan::{Plan, Planning, Role},
    sprout,
    store::CodeMod,
    worker::Status,
};

const ACCENT: Color = Color::Green;
const MUTED: Color = Color::Rgb(161, 170, 160);
const BORDER: Color = Color::Rgb(89, 99, 79);
const CONTROL: Color = Color::Rgb(51, 59, 50);
const SELECTED: Color = Color::Rgb(62, 73, 55);
const USER_BACKGROUND: Color = Color::Rgb(43, 49, 43);
const KEY_HINT: Color = Color::Rgb(161, 170, 160);
const MOD_GLYPH: &str = "◇";
const DIALOG_WIDTH: u16 = 68;

pub fn input() -> TextArea<'static> {
    field("message", "Describe a feature, a fix, or an idea...")
}

pub fn name_input() -> TextArea<'static> {
    field(
        "new codemod",
        "Describe what you want to build or change...",
    )
}

pub fn edit_input() -> TextArea<'static> {
    field("edit queued message", "")
}

pub fn repository_input() -> TextArea<'static> {
    field("GitHub repository", "owner/repository")
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

pub fn draw(
    frame: &mut Frame,
    app: &mut App,
    pose: sprout::Pose,
    elapsed: Option<Duration>,
) -> Rect {
    app.scroll.begin(app.view);
    let area = frame.area().inner(Margin::new(2, 1));
    let composer_view = app.composer_view();
    let show_dock = matches!(composer_view, View::Chat | View::NewMod);
    let dock = app.action_dock();
    let menu_open = matches!(app.view, View::Actions(_));
    let dock_width = area.width.saturating_sub(4);
    let dock_header = dock_header(&dock, dock_width, elapsed, menu_open);
    let dock_hints = dock_hints(app, dock_width, menu_open);
    let read_only = matches!(composer_view, View::Chat) && app.read_only();
    let mut identities: Vec<_> = app.active_workers().collect();
    if identities.is_empty() {
        identities.extend(app.current_worker());
    }
    let identities = Paragraph::new(
        identities
            .iter()
            .map(|worker| {
                Line::from(worker.label(worker.busy().then(|| activity_glyph(elapsed))))
                    .fg(KEY_HINT)
            })
            .collect::<Vec<_>>(),
    )
    .wrap(Wrap { trim: false });
    let header_height = (2 + identities.line_count(area.width.saturating_sub(10)) as u16).max(4);
    let mod_height = u16::from(!matches!(
        composer_view,
        View::NewMod | View::ProjectSetup(false, _)
    ));
    let dock_chrome = dock_header.len() as u16 + dock_hints.len() as u16 + 3;
    let available_rows = area
        .height
        .saturating_sub(header_height + 5 + mod_height + dock_chrome)
        .max(1);
    let dock_input_height = if read_only {
        0
    } else {
        (composer_rows(&app.input, dock_width).clamp(4, 8) as u16).min(available_rows)
    };
    let dock_height = dock_chrome + dock_input_height;
    if area.width < 32
        || area.height < header_height + 5 + mod_height + if show_dock { dock_height } else { 0 }
    {
        frame.render_widget(
            Paragraph::new("Make the terminal a little larger.\nCtrl+C to quit.")
                .wrap(Wrap { trim: false }),
            area,
        );
        return Rect::default();
    }

    let gap = u16::from(area.height >= 26 + 2 * mod_height);
    let [header, _, mod_row, _, content, _, dock_area, input, footer] = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Length(gap),
        Constraint::Length(mod_height),
        Constraint::Length(gap * mod_height),
        Constraint::Min(1),
        Constraint::Length(gap),
        Constraint::Length(if show_dock { dock_height } else { 0 }),
        Constraint::Length(if read_only || show_dock {
            0
        } else {
            composer_rows(&app.input, dock_width).clamp(1, 4) as u16 + 2
        }),
        Constraint::Length(u16::from(!show_dock)),
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
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec!["sprowt".fg(ACCENT).bold(), " harness".bold()]),
            Line::from(fit_name(&project, heading.width)).fg(MUTED),
        ]),
        heading,
    );
    frame.render_widget(
        identities,
        Rect {
            y: heading.y + 2,
            height: heading.height.saturating_sub(2),
            ..heading
        },
    );

    draw_mod_selector(
        frame,
        app.current_mod()
            .map_or("", |code_mod| code_mod.name.as_str()),
        mod_row,
        &app.worker_activity(),
        matches!(
            app.view,
            View::Mods(_) | View::DeleteMod(_) | View::CloseMod(_)
        ),
    );
    if !menu_open && matches!(composer_view, View::Chat | View::EditQueue(_)) {
        let count = if read_only {
            0
        } else {
            app.current_mod().map_or(0, |code_mod| code_mod.queue.len())
        };
        let steering = if read_only {
            0
        } else {
            app.current_mod()
                .map_or(0, |code_mod| code_mod.steering.len())
        };
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
        draw_conversation(frame, app, conversation, elapsed);
        app.scroll.content = conversation;
        draw_queue_preview(frame, app, queue);
        draw_steering_preview(frame, app, steering);
    }
    let dialog_area = Rect {
        y: mod_row.bottom() + gap,
        width: area.width.min(if matches!(app.view, View::Queue(_)) {
            area.width
        } else {
            DIALOG_WIDTH
        }),
        height: area.bottom().saturating_sub(mod_row.bottom() + gap),
        ..area
    };
    if let View::Actions(selected) = app.view {
        draw_actions(
            frame,
            app,
            &dock,
            selected,
            Rect {
                width: dialog_area.width,
                ..content
            },
        );
    } else if let View::Failure(scroll) = app.view {
        draw_failure(
            frame,
            app,
            scroll,
            Rect {
                width: area.width,
                ..dialog_area
            },
        );
    } else if let View::ProjectSetup(saved, scroll) = app.view {
        draw_project_setup(frame, app, saved, scroll, dialog_area);
    } else if let View::Repository(create) = app.view {
        let choices = Paragraph::new(vec![
            Line::from(if create {
                "○ connect existing   ◉ create private"
            } else {
                "◉ connect existing   ○ create private"
            })
            .fg(ACCENT),
            Line::from("An empty repository gets the starting commit before the PR."),
        ])
        .wrap(Wrap { trim: false });
        let rows = choices.line_count(dialog_area.width.saturating_sub(4));
        let (body, _) = draw_dialog(
            frame,
            dialog_area,
            "publish to GitHub",
            rows + 4,
            false,
            &[("tab", "choose"), ("↵", "review"), ("esc", "back")],
        );
        let [choices_area, _, field] = Layout::vertical([
            Constraint::Length((rows as u16).min(body.height.saturating_sub(4))),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .areas(body);
        draw_scrollable_text(frame, app, choices, choices_area);
        frame.render_widget(&app.input, field);
        app.scroll.input = field;
    } else if let View::ConfirmRepository(create) = app.view {
        let slug = app.input.lines().join("\n");
        let text = Paragraph::new(format!(
            "{} github.com/{}?\nThe starting commit is pushed if the repository is empty.",
            if create {
                "Create private repository"
            } else {
                "Connect to"
            },
            slug.trim()
        ))
        .wrap(Wrap { trim: false });
        let rows = text.line_count(dialog_area.width.saturating_sub(4));
        let (body, _) = draw_dialog(
            frame,
            dialog_area,
            "confirm repository",
            rows,
            false,
            &[("↵", "confirm"), ("esc", "back")],
        );
        draw_scrollable_text(frame, app, text, body);
    } else if let View::Network(id, scroll) = app.view {
        draw_network(frame, app, id, scroll, dialog_area);
    } else if let View::Mods(index) = app.view {
        draw_mod_picker(frame, app, index, dialog_area);
    } else if let View::DeleteMod(index) = app.view {
        draw_delete_mod(frame, app, index, dialog_area);
    } else if let View::CloseMod(index) = app.view {
        draw_close_mod(frame, app, index, dialog_area);
    } else if let View::Queue(index) = app.view {
        draw_queue_editor(frame, app, index, dialog_area);
    } else if let View::Review(scroll) = app.view {
        draw_review(
            frame,
            app,
            scroll,
            Rect {
                width: area.width,
                ..dialog_area
            },
        );
    } else if app.view.details_tab().is_some() {
        inspector::draw(
            frame,
            app,
            Rect {
                width: area.width,
                ..dialog_area
            },
        );
    } else if matches!(app.view, View::Publish) {
        let count = app.review.as_ref().map_or(0, |review| review.count());
        let text = Paragraph::new(format!(
            "Publish {count} changed files {}?\n\n{}",
            if app.git_state().is_some_and(|s| s.pr.is_some()) {
                "to the existing PR"
            } else {
                "as a PR"
            },
            app.state_summary().evidence
        ))
        .wrap(Wrap { trim: false });
        let rows = text.line_count(dialog_area.width.saturating_sub(4));
        let (body, _) = draw_dialog(
            frame,
            dialog_area,
            "publish PR?",
            rows,
            false,
            &[("↵", "publish"), ("esc", "cancel")],
        );
        draw_scrollable_text(frame, app, text, body);
    } else if matches!(app.view, View::EditQueue(_)) {
        frame.render_widget(&app.input, input);
        app.scroll.input = input;
        let shortcuts = if footer.width >= 40 {
            "enter save   ctrl+j newline   esc cancel"
        } else {
            "↵ save  esc cancel"
        };
        frame.render_widget(Line::from(shortcuts).fg(MUTED), footer);
    }
    if show_dock {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(BORDER))
            .padding(Padding::horizontal(1));
        let inner = block.inner(dock_area);
        frame.render_widget(block, dock_area);
        let [status, separator, input, hints] = Layout::vertical([
            Constraint::Length(dock_header.len() as u16),
            Constraint::Length(1),
            Constraint::Length(dock_input_height),
            Constraint::Length(dock_hints.len() as u16),
        ])
        .areas(inner);
        frame.render_widget(Paragraph::new(dock_header), status);
        frame.render_widget(
            Line::from(format!(
                "├{}┤",
                "─".repeat(dock_area.width.saturating_sub(2) as usize)
            ))
            .fg(BORDER),
            Rect {
                x: dock_area.x,
                width: dock_area.width,
                ..separator
            },
        );
        if !read_only {
            app.input.set_block(Block::default());
            app.input
                .set_placeholder_text(if matches!(composer_view, View::NewMod) {
                    "Describe your codemod…"
                } else if app.current_mod().and_then(|m| m.question()).is_some() {
                    "Answer the question…"
                } else {
                    "Add an instruction…"
                });
            if menu_open {
                let mut draft = app.input.clone();
                draft.set_cursor_style(Style::new());
                frame.render_widget(&draft, input);
            } else {
                frame.render_widget(&app.input, input);
                app.scroll.input = input;
            }
        }
        frame.render_widget(Paragraph::new(dock_hints), hints);
    }
    app.scroll.finish(app.view);
    heading
}

fn composer_rows(input: &TextArea<'_>, width: u16) -> usize {
    // Keep the trailing empty row where a newline puts the cursor.
    let lines: Vec<_> = input
        .lines()
        .iter()
        .map(|line| Line::raw(line.as_str()))
        .collect();
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .line_count(width)
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
    frame.render_widget(
        Line::from(vec![
            format!("next pass ({count})  ").fg(KEY_HINT),
            "ctrl+q ".fg(ACCENT),
            "manage".fg(KEY_HINT),
        ]),
        title,
    );
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

fn draw_queue_editor(frame: &mut Frame, app: &mut App, index: usize, area: Rect) {
    let count = app.current_mod().map_or(0, |code_mod| code_mod.queue.len());
    let mut hints = vec![
        ("↑↓", "focus"),
        ("space", "mark"),
        (
            "s",
            if app.running_workers().is_empty() {
                "send when workers connect"
            } else {
                "send to workers"
            },
        ),
        ("↵", "edit"),
        ("d", "remove"),
    ];
    if area.width >= 50 {
        hints.extend([("k", "move↑"), ("j", "move↓")]);
    } else {
        hints.push(("k/j", "reorder"));
    }
    let target = app
        .steer_target
        .map_or_else(|| "target: all".into(), |id| format!("target: w{id}"));
    if !app.running_workers().is_empty() {
        hints.push(("t", &target));
    }
    hints.push(("esc", "back"));
    let natural_width = 2 + hints
        .iter()
        .map(|(key, label)| Span::raw(*key).width() + Span::raw(*label).width() + 3)
        .sum::<usize>();
    let area = Rect {
        width: area.width.min((natural_width as u16).max(DIALOG_WIDTH)),
        ..area
    };
    let title = if app.queue_selection.is_empty() {
        format!("queued for next pass ({count})")
    } else {
        format!(
            "queued for next pass ({count}) · {} selected",
            app.queue_selection.len()
        )
    };
    let (messages, _) = draw_dialog(frame, area, &title, count, false, &hints);
    app.scroll.content = messages;
    frame.render_stateful_widget(
        List::new(queue_items(app, messages.width)).highlight_style(dialog_selection()),
        messages,
        &mut ListState::default().with_selected((count > 0).then_some(index)),
    );
}

fn draw_mod_selector(frame: &mut Frame, name: &str, area: Rect, workers: &[String], open: bool) {
    let [label, selector, status] = Layout::horizontal([
        Constraint::Length(11),
        Constraint::Length(area.width.saturating_sub(11).min(54)),
        Constraint::Min(0),
    ])
    .areas(area);
    if !workers.is_empty() {
        let activity = format!("  {}", workers.join(" │ "));
        let count = workers.len();
        let label = if Line::from(activity.clone()).width() <= status.width as usize {
            activity
        } else {
            format!(
                "  {count} {} running",
                if count == 1 { "worker" } else { "workers" }
            )
        };
        frame.render_widget(Line::from(label).fg(ACCENT), status);
    }
    frame.render_widget(Line::from("<codemod/>").fg(KEY_HINT), label);
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

fn draw_mod_picker(frame: &mut Frame, app: &mut App, index: usize, area: Rect) {
    let indices = app.picker_indices();
    let count = indices.len();
    let mut hints = Vec::new();
    if count > 0 {
        hints.push(("↑↓", "select"));
    }
    hints.push(("↵", if index < count { "open" } else { "new" }));
    if index < count {
        if !app.show_closed {
            hints.push(("c", "close"));
        }
        if app.show_closed && app.mods[indices[index]].git_root.is_some() {
            hints.push(("r", "reopen"));
        }
        hints.push(("d", "delete"));
    }
    hints.push(("tab", if app.show_closed { "active" } else { "closed" }));
    hints.push(("esc", "back"));
    let (list_area, new_mod) = draw_dialog(
        frame,
        area,
        &format!(
            "{} codemods ({count})",
            if app.show_closed { "closed" } else { "active" }
        ),
        count,
        true,
        &hints,
    );
    app.scroll.content = list_area.union(new_mod);
    let items = indices.iter().map(|&i| {
        let code_mod = &app.mods[i];
        let active = app
            .current_mod()
            .is_some_and(|active| active.id == code_mod.id);
        Line::from(vec![
            format!("{} ", if code_mod.closed { "□" } else { MOD_GLYPH }).fg(ACCENT),
            Span::raw(fit_name(&code_mod.name, list_area.width.saturating_sub(4))),
            (if active { " ✓" } else { "" }).fg(ACCENT),
        ])
    });
    let selected = dialog_selection();
    let highlight = if index < count {
        selected
    } else {
        Style::new()
    };
    if count == 0 {
        frame.render_widget(
            Line::from(if app.show_closed {
                "No closed codemods"
            } else {
                "No active codemods"
            })
            .fg(KEY_HINT),
            list_area,
        );
    } else {
        frame.render_stateful_widget(
            List::new(items).highlight_style(highlight),
            list_area,
            &mut ListState::default().with_selected(Some(index.min(count - 1))),
        );
    }
    let action = Line::from(vec!["+ ".fg(ACCENT), "new codemod".into()]);
    frame.render_widget(
        if index == count {
            action.style(selected)
        } else {
            action
        },
        new_mod,
    );
}

fn draw_delete_mod(frame: &mut Frame, app: &mut App, index: usize, area: Rect) {
    let code_mod = &app.mods[index];
    let removal = if code_mod.git_root.is_some() {
        "Permanently deletes local history and discards unpublished work. Existing PRs stay on GitHub."
    } else if code_mod.execution.is_some() {
        "Stops workers; deletes history, the VM and working folder."
    } else if app.has_worker(code_mod.id) {
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
        "delete codemod?",
        rows,
        false,
        &[("↵", "delete"), ("esc", "cancel")],
    );
    draw_scrollable_text(frame, app, body, content);
}

fn draw_close_mod(frame: &mut Frame, app: &mut App, index: usize, area: Rect) {
    let body = Paragraph::new(vec![
        Line::from(fit_name(
            &app.mods[index].name,
            area.width.saturating_sub(6),
        ))
        .bold(),
        Line::from("Save a checkpoint; keep the worktree and history; remove the VM.").fg(KEY_HINT),
    ])
    .wrap(Wrap { trim: false });
    let (content, _) = draw_dialog(
        frame,
        area,
        "close codemod?",
        body.line_count(area.width.saturating_sub(4)),
        false,
        &[("↵", "close"), ("esc", "cancel")],
    );
    draw_scrollable_text(frame, app, body, content);
}

fn draw_review(frame: &mut Frame, app: &mut App, scroll: u16, area: Rect) {
    let Some(review) = &app.review else {
        return;
    };
    let mut keys = vec![("↑↓", "scroll"), (page_key(), "page")];
    if app.can_publish() {
        keys.extend([("r", "ask agent to review"), ("p", "publish PR")]);
    }
    keys.push(("esc", "back"));
    let hints = key_hints(&keys);
    let [panel, footer] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(hints.line_count(area.width) as u16),
    ])
    .areas(area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER))
        .padding(Padding::horizontal(1))
        .title(
            Line::from(format!(" diff · {} files ", review.count()))
                .fg(KEY_HINT)
                .bold(),
        );
    let inner = block.inner(panel);
    app.scroll.content = inner;
    app.page_size = inner.height.max(1);
    let mut lines = Text::from(review.summary.as_str()).lines;
    lines.push(Line::default());
    if review.patch.is_empty() {
        lines.push(Line::from("No file changes."));
    } else {
        lines.extend(review.patch.lines().map(|line| {
            let style = if line.starts_with("diff --git") {
                Style::new().fg(KEY_HINT).bold()
            } else if line.starts_with('+') {
                Style::new().fg(ACCENT)
            } else if line.starts_with('-') {
                Style::new().fg(Color::Red)
            } else if line.starts_with("@@") {
                Style::new().fg(Color::Cyan)
            } else {
                Style::new()
            };
            Line::from(line).style(style)
        }));
    }
    let body = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(block);
    let max_scroll = body
        .line_count(inner.width)
        .saturating_sub(inner.height as usize)
        .min(u16::MAX as usize) as u16;
    let scroll = scroll.min(max_scroll);
    app.view = View::Review(scroll);
    frame.render_widget(body.scroll((scroll, 0)), panel);
    frame.render_widget(hints, footer);
}

fn draw_history(frame: &mut Frame, app: &mut App, scroll: u16, area: Rect, task: Option<i64>) {
    if task.is_some_and(|id| app.inspect_task(id).is_none()) {
        tasks::draw_tasks(frame, app, 0, area);
        return;
    }
    let Some(code_mod) = app.current_mod() else {
        return;
    };
    let mut keys = vec![("↑↓", "scroll"), (page_key(), "page")];
    if task.is_some() {
        keys.push(("a", "all history"));
    }
    keys.extend([("esc", "back"), ("ctrl+t", "conversation")]);
    let hints = key_hints(&keys);
    let [panel, footer] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(hints.line_count(area.width) as u16),
    ])
    .areas(area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER))
        .padding(Padding::horizontal(1))
        .title(
            Line::from(task.and_then(|id| app.inspect_task(id)).map_or_else(
                || " all history ".into(),
                |task| format!(" task {} · history ", task.number),
            ))
            .fg(KEY_HINT)
            .bold(),
        );
    let inner = block.inner(panel);
    let mut rows = Vec::new();
    let mut total: usize = 0;
    for item in conversation_blocks(code_mod, false, true, inner.width, None, &[], task) {
        let paragraph = Paragraph::new(item.text).wrap(Wrap { trim: false });
        let height =
            paragraph.line_count(inner.width.saturating_sub(if item.user { 2 } else { 0 }));
        rows.push((total, height, item.user, paragraph));
        total += height + 1;
    }
    if total == 0 {
        let paragraph = Paragraph::new(if task.is_some() {
            "No messages linked to this task yet. Older messages are in All history."
        } else {
            "No worker history yet."
        })
        .wrap(Wrap { trim: false });
        let height = paragraph.line_count(inner.width);
        rows.push((0, height, false, paragraph));
        total = height;
    }
    let max_scroll = total
        .saturating_sub(1)
        .saturating_sub(inner.height as usize)
        .min(u16::MAX as usize) as u16;
    let scroll = scroll.min(max_scroll);
    frame.render_widget(block, panel);
    draw_message_rows(frame, rows, inner, scroll as usize);
    app.scroll.content = inner;
    app.page_size = inner.height.max(1);
    app.view = task.map_or(View::History(scroll), |id| View::TaskHistory(id, scroll));
    frame.render_widget(hints, footer);
}

fn draw_project_setup(frame: &mut Frame, app: &mut App, saved: bool, scroll: u16, area: Rect) {
    let mut lines = vec![
        Line::from(if saved {
            "Use the saved starting files as the Git baseline; keep the finished work."
        } else {
            "Create the initial Git commit from these starting files."
        }),
        Line::default(),
    ];
    if app.setup_files.is_empty() {
        lines.push(Line::from("Empty starting point · create an empty commit").fg(KEY_HINT));
    } else {
        lines.extend(
            app.setup_files
                .iter()
                .map(|path| Line::from(format!("  {path}"))),
        );
    }
    let text = Paragraph::new(lines).wrap(Wrap { trim: false });
    let rows = text.line_count(area.width.saturating_sub(4));
    let (body, _) = draw_dialog(
        frame,
        area,
        "set up Git",
        rows,
        false,
        &[
            ("↑↓", "files"),
            ("↵", if saved { "adopt" } else { "set up + plan" }),
            ("esc", "back"),
        ],
    );
    let scroll = scroll.min(
        rows.saturating_sub(body.height as usize)
            .min(u16::MAX as usize) as u16,
    );
    app.view = View::ProjectSetup(saved, scroll);
    app.scroll.content = body;
    app.page_size = body.height.max(1);
    frame.render_widget(text.scroll((scroll, 0)), body);
}

fn dialog_selection() -> Style {
    Style::new().fg(Color::White).bg(SELECTED)
}

fn draw_network(frame: &mut Frame, app: &mut App, id: i64, scroll: u16, area: Rect) {
    let Some(access) = app
        .network_requests
        .iter()
        .find(|r| r.id == id && r.status == "pending")
    else {
        return;
    };
    let mut lines = vec![
        Line::from(format!(
            "w{} requests access for this codemod",
            access.worker
        ))
        .fg(KEY_HINT),
        Line::default(),
    ];
    lines.extend(
        access
            .domains
            .iter()
            .map(|d| Line::from(format!("◇ {d}")).fg(ACCENT).bold()),
    );
    lines.push(Line::default());
    lines.push(Line::from(access.reason.clone()));
    let body = Paragraph::new(lines).wrap(Wrap { trim: false });
    let rows = body.line_count(area.width.saturating_sub(4));
    let mut shortcuts = vec![("a", "allow for codemod"), ("d", "deny"), ("esc", "back")];
    let (mut content, _) = draw_dialog(frame, area, "network access", rows, false, &shortcuts);
    if rows > content.height as usize {
        shortcuts.insert(2, ("↑↓", "scroll"));
        content = draw_dialog(frame, area, "network access", rows, false, &shortcuts).0;
    }
    let maximum = body
        .line_count(content.width)
        .saturating_sub(content.height as usize)
        .min(u16::MAX as usize) as u16;
    let scroll = scroll.min(maximum);
    app.view = View::Network(id, scroll);
    app.scroll.content = content;
    frame.render_widget(body.scroll((scroll, 0)), content);
}

fn draw_scrollable_text(frame: &mut Frame, app: &mut App, text: Paragraph<'_>, area: Rect) {
    let maximum = text
        .line_count(area.width)
        .saturating_sub(area.height as usize)
        .min(u16::MAX as usize) as u16;
    app.scroll.offset = app.scroll.offset.min(maximum);
    app.scroll.content = area;
    frame.render_widget(text.scroll((app.scroll.offset, 0)), area);
}

fn key_hints(shortcuts: &[(&str, &str)]) -> Paragraph<'static> {
    let mut hints = Vec::new();
    for (index, (key, label)) in shortcuts.iter().enumerate() {
        if index > 0 {
            hints.push(Span::raw("  "));
        }
        // Keep each key and its description together when wrapping.
        hints.push(format!("{key}\u{a0}").fg(Color::White).bold());
        hints.push(label.replace(' ', "\u{a0}").fg(KEY_HINT));
    }
    Paragraph::new(Line::from(hints)).wrap(Wrap { trim: false })
}

fn page_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "fn+↑/↓"
    } else {
        "pgup/pgdn"
    }
}

fn draw_dialog(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    rows: usize,
    has_action: bool,
    shortcuts: &[(&str, &str)],
) -> (Rect, Rect) {
    let hints = key_hints(shortcuts);
    let hints_height = hints.line_count(area.width.saturating_sub(4)) as u16;
    let action_height = u16::from(has_action);
    let panel = Rect {
        height: (rows.clamp(1, 12) as u16 + action_height + 3 + hints_height).min(area.height),
        ..area
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER))
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
        .fg(BORDER),
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

fn wrap_line(line: Line<'static>, width: u16, indent: usize) -> Vec<Line<'static>> {
    if line.width() <= width as usize || width == 0 {
        return vec![line];
    }
    let glyphs: Vec<_> = line
        .styled_graphemes(Style::new())
        .map(|g| Span::styled(g.symbol.to_owned(), g.style))
        .collect();
    let mut lines = Vec::new();
    let mut start = 0;
    while start < glyphs.len() {
        let padding = if start == 0 {
            0
        } else {
            indent.min(width.saturating_sub(2) as usize)
        };
        let mut used = padding;
        let mut end = start;
        while end < glyphs.len() && used + glyphs[end].width() <= width as usize {
            used += glyphs[end].width();
            end += 1;
        }
        if end < glyphs.len()
            && let Some(space) = (start..end).rev().find(|i| {
                glyphs[*i].content == " " && glyphs[start..*i].iter().any(|g| g.content != " ")
            })
        {
            end = space;
        }
        end = end.max(start + 1);
        let mut spans = vec![Span::raw(" ".repeat(padding))];
        for glyph in &glyphs[start..end] {
            if let Some(last) = spans.last_mut().filter(|last| last.style == glyph.style) {
                last.content.to_mut().push_str(&glyph.content);
            } else {
                spans.push(glyph.clone());
            }
        }
        lines.push(Line::from(spans));
        start = end;
        while start < glyphs.len() && glyphs[start].content == " " {
            start += 1;
        }
    }
    lines
}

fn action_control(item: &ActionItem, width: u16, menu: bool) -> Line<'static> {
    let mut spans = Vec::new();
    let ctrl = if width < 60 { "^" } else { "ctrl+" };
    let shortcut = item
        .action
        .shortcut()
        .map(|key| format!("{ctrl}{key}"))
        .or_else(|| {
            item.action.menu_shortcut().map(|key| {
                if menu {
                    key.to_string()
                } else {
                    format!("{ctrl}g {key}")
                }
            })
        });
    if let Some(shortcut) = shortcut {
        let column = if menu {
            if width < 60 { 2 } else { 6 }
        } else {
            0
        };
        spans.push(format!("{shortcut:>column$} ").fg(ACCENT));
    }
    spans.push(item.label.clone().fg(KEY_HINT));
    Line::from(spans)
}

fn dock_header(
    dock: &ActionDock,
    width: u16,
    elapsed: Option<Duration>,
    menu_open: bool,
) -> Vec<Line<'static>> {
    let (glyph, color) = match dock.tone {
        Tone::Quiet => ("◇", KEY_HINT),
        Tone::Busy => (activity_glyph(elapsed), KEY_HINT),
        Tone::Attention => ("!", Color::Red),
        Tone::Ready => ("✓", ACCENT),
    };
    let status = Line::from(format!("{glyph} {}", dock.status)).fg(color);
    let primary = (!menu_open)
        .then(|| {
            dock.primary
                .and_then(|id| dock.actions.iter().find(|a| a.action == id))
        })
        .flatten()
        .map(|item| {
            let mut control = action_control(item, width, false);
            for span in &mut control.spans {
                span.style = span.style.bold().bg(CONTROL);
            }
            control
        })
        .unwrap_or_default();
    let mut lines = dock_row(status, primary, width);
    if !menu_open && !dock.detail.is_empty() {
        lines.extend(
            wrap_line(Line::from(dock.detail.clone()).fg(KEY_HINT), width, 0)
                .into_iter()
                .take(2),
        );
    }
    if !menu_open && !dock.evidence.is_empty() {
        lines.extend(wrap_line(
            Line::from(dock.evidence.clone()).fg(KEY_HINT),
            width,
            0,
        ));
    }
    if !menu_open && let Some(error) = &dock.error {
        let mut error = wrap_line(Line::from(error.clone()).fg(Color::Red), width, 0);
        if error.len() > 2 {
            error[1] = Line::from(format!(
                "{}…",
                fit_name(&error[1].to_string(), width.saturating_sub(1))
            ))
            .fg(Color::Red);
        }
        lines.extend(error.into_iter().take(2));
    }
    lines
}

fn dock_hints(app: &App, width: u16, menu_open: bool) -> Vec<Line<'static>> {
    let hint = |key: &str, label: String| {
        Line::from(vec![format!("{key} ").fg(ACCENT), label.fg(KEY_HINT)])
    };
    if menu_open {
        return vec![hint("esc", "Back".into())];
    }
    let new_mod = matches!(app.composer_view(), View::NewMod);
    let ctrl = if width < 60 { "^" } else { "ctrl+" };
    let mut actions = Line::default();
    if !new_mod {
        if app
            .action_dock()
            .actions
            .iter()
            .any(|a| a.action == Action::Stop)
        {
            actions = hint(&format!("{ctrl}r"), "Pause".into());
            actions.spans.push(Span::raw("   "));
        }
        actions
            .spans
            .extend(hint(&format!("{ctrl}t"), "Details".into()).spans);
        actions.spans.push(Span::raw("   "));
    }
    actions
        .spans
        .extend(hint(&format!("{ctrl}g"), "More".into()).spans);
    if !new_mod && app.read_only() {
        return dock_row(Line::default(), actions, width);
    }
    let send = if new_mod {
        "Create codemod".into()
    } else if let Some(question) = app.current_mod().and_then(|m| m.question()) {
        format!("Answer #{}", question.id)
    } else if app.published() || app.version_ready() {
        "Request edits".into()
    } else {
        "Queue for next pass".into()
    };
    let mut input = hint("↵", send);
    let newline = hint(&format!("{ctrl}j"), "Newline".into());
    let mut lines = Vec::new();
    if input.width() + newline.width() + 3 <= width as usize {
        input.spans.push(Span::raw("   "));
        input.spans.extend(newline.spans);
    } else {
        lines.extend(wrap_line(input, width, 0));
        input = newline;
    }
    lines.extend(dock_row(input, actions, width));
    lines
}

fn dock_row(left: Line<'static>, right: Line<'static>, width: u16) -> Vec<Line<'static>> {
    if right.width() == 0 {
        return wrap_line(left, width, 0);
    }
    if left.width() + right.width() + 3 <= width as usize {
        let gap = width as usize - left.width() - right.width();
        let mut row = left;
        row.spans.push(Span::raw(" ".repeat(gap)));
        row.spans.extend(right.spans);
        return vec![row];
    }
    let mut lines = wrap_line(left, width, 0);
    lines.extend(
        wrap_line(right, width, 0)
            .into_iter()
            .map(Line::right_aligned),
    );
    lines
}

fn draw_actions(frame: &mut Frame, app: &mut App, dock: &ActionDock, selected: Action, area: Rect) {
    frame.render_widget(ratatui::widgets::Clear, area);
    let actions = dock.menu_actions();
    let (body, _) = draw_dialog(
        frame,
        area,
        "actions",
        actions.len() + 8,
        false,
        &[("↑↓", "select"), ("↵", "choose"), ("esc", "back")],
    );
    app.scroll.content = body;
    let mut previous = None;
    let items = actions.iter().map(|item| {
        let group = item.action.group();
        let mut lines = Vec::new();
        if previous != Some(group) {
            if previous.is_some() {
                lines.push(Line::default());
            }
            lines.push(
                Line::from(["Work", "Inspect", "Codemod", "Remove"][group as usize])
                    .fg(KEY_HINT)
                    .bold(),
            );
            previous = Some(group);
        }
        let mut line = action_control(item, body.width, true);
        if dock.primary == Some(item.action) {
            line.spans.insert(0, "› ".fg(ACCENT));
        } else {
            line.spans.insert(0, Span::raw("  "));
        }
        lines.push(line);
        ratatui::widgets::ListItem::new(lines)
    });
    let index = actions.iter().position(|a| a.action == selected);
    frame.render_stateful_widget(
        List::new(items).highlight_style(dialog_selection()),
        body,
        &mut ListState::default().with_selected(index),
    );
}

fn draw_failure(frame: &mut Frame, app: &mut App, scroll: u16, area: Rect) {
    let hints = key_hints(&[("↑↓", "scroll"), ("esc", "back")]);
    let [panel, footer] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(hints.line_count(area.width) as u16),
    ])
    .areas(area);
    app.scroll.content = panel;
    app.page_size = panel.height.saturating_sub(2).max(1);
    let body = Paragraph::new(app.action_error().unwrap_or("No current error."))
        .wrap(Wrap { trim: false })
        .block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(BORDER))
                .padding(Padding::horizontal(1))
                .title(Line::from(" error details ").fg(KEY_HINT).bold()),
        );
    let maximum = body
        .line_count(panel.width.saturating_sub(4))
        .saturating_sub(panel.height.saturating_sub(2) as usize)
        .min(u16::MAX as usize) as u16;
    frame.render_widget(body.scroll((scroll.min(maximum), 0)), panel);
    app.view = View::Failure(scroll.min(maximum));
    frame.render_widget(hints, footer);
}

fn activity_glyph(elapsed: Option<Duration>) -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    elapsed.map_or("⠿", |elapsed| {
        FRAMES[(elapsed.as_millis() / 80 % FRAMES.len() as u128) as usize]
    })
}

fn command_lines(argv: &[String], width: u16) -> Vec<Line<'static>> {
    let mut blocks = Vec::new();
    let multiline = argv.iter().filter(|arg| arg.contains('\n')).count();
    let quote = |arg: &String| {
        if !arg.is_empty()
            && arg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_./:=+-@".contains(c))
        {
            arg.clone()
        } else {
            format!("'{}'", arg.replace('\'', "'\\''"))
        }
    };
    let arguments = argv
        .iter()
        .map(|arg| {
            if arg.contains('\n') {
                blocks.push(arg);
                if multiline == 1 {
                    "‹code›".into()
                } else {
                    format!("‹code {}›", blocks.len())
                }
            } else {
                quote(arg)
            }
        })
        .collect::<Vec<_>>();
    let command = format!("$ {}", arguments.join(" "));
    let content =
        std::iter::once(command.as_str()).chain(blocks.iter().flat_map(|block| block.lines()));
    let capacity = width.saturating_sub(9).max(1) as usize;
    let mut lines = Vec::new();
    for line in content {
        let line = line.replace('\t', "    ");
        let indent = line
            .chars()
            .take_while(|c| *c == ' ')
            .count()
            .min(capacity / 2);
        let mut row = String::new();
        let mut used = 0;
        for glyph in Span::raw(&line).styled_graphemes(Style::new()) {
            let size = Span::raw(glyph.symbol).width();
            if used + size > capacity && !row.is_empty() {
                if let Some(split) = row.rfind(' ').filter(|index| *index > indent) {
                    let remainder = row[split + 1..].to_owned();
                    lines.push(row[..split].to_owned());
                    row = " ".repeat(indent) + &remainder;
                    used = Span::raw(&row).width();
                } else {
                    lines.push(row);
                    row = " ".repeat(indent);
                    used = indent;
                }
            }
            row.push_str(glyph.symbol);
            used += size;
        }
        lines.push(row);
    }
    lines
        .into_iter()
        .map(|line| Line::from(vec!["       │ ".fg(BORDER), line.fg(KEY_HINT)]))
        .collect()
}

fn plan_lines(
    plan: &Plan,
    planning: &Planning,
    execution: Option<&Execution>,
    details: bool,
    width: u16,
    activity: Option<&'static str>,
    network: &[crate::network::Access],
) -> Vec<Line<'static>> {
    let completed = execution
        .is_some_and(|e| e.complete() && matches!(e.status.as_str(), "review" | "applied"));
    let compact = completed && !details;
    let heading = if planning.source.starts_with("upstream:") {
        "▤ Integration plan"
    } else {
        "▤ Plan"
    };
    let toggle = Line::from(vec![
        "ctrl+o ".fg(ACCENT),
        if details {
            "▾ hide details"
        } else {
            "▸ show details"
        }
        .fg(KEY_HINT),
    ]);
    let mut title = Line::from(heading.fg(ACCENT).bold());
    let mut lines = if title.width() + toggle.width() + 2 <= width as usize {
        title.spans.push(Span::raw("  "));
        title.spans.extend(toggle.spans);
        vec![title]
    } else {
        vec![title, toggle]
    };
    if completed && let Some(execution) = execution {
        lines.push(
            Line::from(format!(
                "✓ {}/{} checks passed",
                execution
                    .checks
                    .iter()
                    .filter(|c| c.exit_code == Some(0))
                    .count(),
                execution.check_count(plan)
            ))
            .fg(ACCENT),
        );
        if compact {
            lines.push(Line::default());
        }
    }
    if details {
        for (label, notes) in [
            ("contracts", &plan.contracts),
            ("assumptions", &plan.assumptions),
            ("outside scope", &plan.non_goals),
        ] {
            if !notes.is_empty() {
                lines.push(Line::from(label).fg(KEY_HINT));
                lines.extend(
                    notes
                        .iter()
                        .flat_map(|note| wrap_line(Line::from(format!("     · {note}")), width, 7)),
                );
            }
        }
    }
    for (index, task) in plan.tasks.iter().enumerate() {
        let run = execution
            .and_then(|execution| execution.tasks.iter().find(|run| run.task_id == task.id));
        let marker = run.map_or("○", |run| match run.status.as_str() {
            "done" => "✓",
            "running" | "sending" | "checking" if activity.is_some() => activity.unwrap(),
            "running" | "sending" => "●",
            "checking" => "◌",
            "blocked" | "paused" | "repair_paused" => "!",
            "waiting" => "?",
            "repair_wait" => "↺",
            _ => "○",
        });
        let number_width = plan.tasks.len().to_string().len();
        let prefix = format!("{marker} {:>number_width$}. ", index + 1);
        let indent = prefix.len() - marker.len() + 1;
        let command_width = width.saturating_sub(number_width.saturating_sub(1) as u16);
        let task_start = lines.len();
        if !compact {
            lines.push(Line::default());
        }
        let title = task.title.clone();
        lines.push(Line::from(vec![
            prefix
                .fg(if marker == "!" { Color::Red } else { ACCENT })
                .bold(),
            title.bold(),
            run.and_then(|r| r.worker)
                .filter(|_| {
                    matches!(
                        run.unwrap().status.as_str(),
                        "running" | "sending" | "checking"
                    )
                })
                .map_or(String::new(), |id| format!(" · w{id}"))
                .fg(KEY_HINT),
        ]));
        if !compact {
            lines.push(Line::from(format!("     {}", task.outcome)));
        }
        if let Some(access) = run.and_then(|run| {
            network
                .iter()
                .filter(|r| r.task == run.id)
                .min_by_key(|r| match r.status.as_str() {
                    "pending" => 0,
                    "approved" => 1,
                    _ => 2,
                })
        }) {
            lines.push(
                Line::from(match access.status.as_str() {
                    "pending" => "     ! Network access needed",
                    "denied" => "     ! Network access denied · task paused",
                    _ => "     ◌ Access approved · reconnecting worker",
                })
                .fg(ACCENT)
                .bold(),
            );
        }
        let dependencies: Vec<_> = task
            .depends_on
            .iter()
            .filter_map(|id| plan.tasks.iter().position(|task| &task.id == id))
            .map(|index| (index + 1).to_string())
            .collect();
        if !compact && !dependencies.is_empty() {
            lines.push(
                Line::from(format!(
                    "     after {} {}",
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
            if let Some(provider) = run.and_then(|run| run.provider.as_deref()) {
                let reason = &run.unwrap().assignment_reason;
                lines.push(
                    Line::from(format!(
                        "     worker · {provider}{}",
                        if reason.is_empty() {
                            String::new()
                        } else {
                            format!(" · {reason}")
                        }
                    ))
                    .fg(KEY_HINT),
                );
            } else if task.worker == "auto" {
                lines.push(Line::from("     worker · assigned when ready").fg(KEY_HINT));
            } else {
                lines.push(
                    Line::from(format!(
                        "     worker · {}{}",
                        task.worker,
                        task.provider_reason
                            .as_ref()
                            .map_or(String::new(), |r| format!(" · {r}"))
                    ))
                    .fg(KEY_HINT),
                );
            }
            if let Some(run) = run
                && run.status != "done"
                && let Some(repair) = &run.repair
                && (run.task_id == repair.task || run.status == "repair_wait")
            {
                lines.push(Line::from(format!("     repair check · {}", repair.check)).fg(ACCENT));
                lines.extend(command_lines(&repair.command, command_width));
                lines.extend(
                    repair
                        .evidence
                        .lines()
                        .take(6)
                        .map(|line| Line::from(format!("       {line}")).fg(KEY_HINT)),
                );
            }
            if let Some(selection) = run.and_then(|run| run.selection.as_ref()) {
                lines.push(
                    Line::from(format!(
                        "     model · {} · {}",
                        selection.model, selection.effort
                    ))
                    .fg(KEY_HINT),
                );
                lines.push(Line::from(format!("     {}", selection.display_reason())).fg(MUTED));
            }
            if !task.files.is_empty() {
                lines.push(
                    Line::from(format!("     files · {}", task.files.join(", "))).fg(KEY_HINT),
                );
            }
            for peer in plan.peers(task) {
                let number = plan.tasks.iter().position(|t| t.id == peer.task).unwrap() + 1;
                lines.push(
                    Line::from(format!(
                        "     with task {number} · {}",
                        peer.topics.join("; ")
                    ))
                    .fg(KEY_HINT),
                );
            }
            if let Some(run) = run
                && !run.checks.is_empty()
            {
                let passed = run
                    .checks
                    .iter()
                    .filter(|check| check.exit_code == Some(0))
                    .count();
                let total = run.checks.len().max(task.checks.len());
                let failed = run.checks.iter().filter(|check| check.failed()).count();
                lines.push(
                    Line::from(format!(
                        "     verification · {passed}/{total} passed · {failed} failed · {} not run",
                        total - passed - failed
                    ))
                    .fg(if passed == total { ACCENT } else { Color::Red }),
                );
            }
            lines.push(Line::from("     checks").fg(KEY_HINT));
            for check in &task.checks {
                let results = run
                    .map(|run| {
                        run.checks
                            .iter()
                            .filter(|result| &result.check == check)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                lines.push(Line::from(format!(
                    "     {} {check}{}",
                    if results.is_empty() {
                        "·"
                    } else if results.iter().all(|result| result.exit_code == Some(0)) {
                        "✓"
                    } else if results.iter().any(|result| result.failed()) {
                        "!"
                    } else {
                        "·"
                    },
                    if results.is_empty() && run.is_some_and(|run| !run.checks.is_empty()) {
                        " · not run"
                    } else if results.iter().any(|result| result.exit_code.is_none()) {
                        " · incomplete"
                    } else {
                        ""
                    }
                )));
                for result in results {
                    lines.extend(command_lines(&result.command, command_width));
                    if result.failed() {
                        lines.extend(
                            result
                                .evidence()
                                .lines()
                                .map(|line| Line::from(format!("       {line}")).fg(Color::Red)),
                        );
                    }
                }
            }
            if let Some(run) = run
                && !run.summary.is_empty()
            {
                lines.push(Line::from(format!("     worker report · {}", run.summary)).fg(MUTED));
            }
            if let Some(run) = run
                && run.status != "done"
                && run.checks.is_empty()
                && !run.verification_feedback.is_empty()
            {
                lines
                    .push(Line::from("     previous verification · recovery context").fg(KEY_HINT));
                for check in run
                    .verification_feedback
                    .iter()
                    .filter(|check| check.failed())
                {
                    lines.push(Line::from(format!("     ! {}", check.check)).fg(Color::Red));
                    lines.extend(
                        check
                            .evidence()
                            .lines()
                            .map(|line| Line::from(format!("       {line}")).fg(Color::Red)),
                    );
                }
            }
        }
        let task_lines = lines.drain(task_start..).collect::<Vec<_>>();
        for mut line in task_lines {
            let text = line.to_string();
            let leading = text.chars().take_while(|c| *c == ' ').count();
            if leading > 0 && indent > 5 {
                line.spans.insert(0, Span::raw(" ".repeat(indent - 5)));
            }
            let continuation = if leading == 0 {
                indent
            } else {
                leading + indent - 5
            };
            lines.extend(wrap_line(line, width, continuation));
        }
    }

    if details {
        if let Some(execution) = execution
            && !execution.checks.is_empty()
        {
            if !completed {
                lines.push(
                    Line::from(format!(
                        "final verification · {} / {} passed",
                        execution
                            .checks
                            .iter()
                            .filter(|check| check.exit_code == Some(0))
                            .count(),
                        execution.check_count(plan)
                    ))
                    .fg(KEY_HINT),
                );
            }
            if let Some(check) = execution.checks.iter().find(|check| check.failed()) {
                lines.extend(wrap_line(
                    Line::from(format!("! {}", check.check)).fg(Color::Red),
                    width,
                    2,
                ));
                lines.extend(command_lines(&check.command, width));
                lines.extend(check.evidence().lines().flat_map(|line| {
                    wrap_line(Line::from(format!("  {line}")).fg(Color::Red), width, 2)
                }));
            }
            let skipped = execution
                .checks
                .iter()
                .filter(|check| check.skipped())
                .count();
            if skipped > 0 {
                lines.push(Line::from(format!("  · {skipped} commands not run")).fg(MUTED));
            }
        }
        if let Some(model) = &planning.model {
            lines.push(
                Line::from(format!(
                    "planner · codex · {model} · {}",
                    planning.effort.as_deref().unwrap_or("medium")
                ))
                .fg(KEY_HINT),
            );
        }
        if let Some(routing) = &planning.routing {
            lines.push(Line::from(routing.clone()).fg(MUTED));
        }
    }
    lines
}

struct ConversationBlock<'a> {
    text: Text<'a>,
    user: bool,
    plan: bool,
}

fn conversation_blocks<'a>(
    code_mod: &'a CodeMod,
    details: bool,
    history: bool,
    width: u16,
    activity: Option<&'static str>,
    network: &[crate::network::Access],
    task_filter: Option<i64>,
) -> Vec<ConversationBlock<'a>> {
    let saved = code_mod
        .planning
        .as_ref()
        .filter(|p| p.status == "ready")
        .and_then(|p| p.plan.as_ref().map(|plan| (p, plan)));
    let plan_id = saved.map(|(p, _)| format!("plan:{}", p.source));
    let mut blocks: Vec<_> = code_mod
        .messages
        .iter()
        .filter_map(|message| {
            if task_filter.is_some() && message.task != task_filter {
                return None;
            }
            let is_plan = plan_id.is_some() && message.item_id == plan_id;
            if history
                && message
                    .item_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("plan:"))
            {
                return None;
            }
            if let Some((planning, plan)) = saved
                && is_plan
            {
                return Some(ConversationBlock {
                    text: plan_lines(
                        plan,
                        planning,
                        code_mod.execution.as_ref(),
                        details,
                        width,
                        activity,
                        network,
                    )
                    .into(),
                    user: false,
                    plan: true,
                });
            }
            if !history
                && (message.role == "codex"
                    || message.role.starts_with("codex:")
                    || message.role.starts_with("muse:")
                    || message.role == "reviewer"
                    || saved.is_some() && message.role == "planner")
            {
                return None;
            }
            let user = message.role == "user";
            let mut text = Text::from(message.body.as_str());
            if !user {
                text.lines.insert(
                    0,
                    Line::from(format!(
                        "{}{}{}",
                        if message.role == "harness" {
                            "◇ sprowt".to_owned()
                        } else if message.role == "reviewer" {
                            "◇ codex · reviewer".to_owned()
                        } else if message.role == "planner" {
                            "▤ codex · planner".to_owned()
                        } else {
                            message.role.split_once(':').map_or_else(
                                || format!("◆ {} · executor", message.role),
                                |(provider, id)| format!("◆ {provider} · executor · w{id}"),
                            )
                        },
                        message
                            .model
                            .as_ref()
                            .map_or(String::new(), |model| format!(" · {model}")),
                        message
                            .effort
                            .as_deref()
                            .or_else(|| message.model.as_ref().map(|_| "effort unknown"))
                            .map_or(String::new(), |effort| format!(" · {effort}"))
                    ))
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
        .collect();
    if !history && let Some(review) = &code_mod.agent_review {
        let stale = review.status != "fixing" && !review.current(code_mod);
        let mut lines = vec![
            Line::from(format!(
                "{} codex · reviewer · gpt-6.1-sol · xhigh",
                if matches!(review.status.as_str(), "pending" | "running") {
                    activity.unwrap_or("◇")
                } else {
                    "◇"
                }
            ))
            .fg(ACCENT)
            .bold(),
            Line::from(if stale {
                "review outdated".into()
            } else if review.status == "fixing"
                && code_mod
                    .execution
                    .as_ref()
                    .is_some_and(|e| e.status == "blocked")
            {
                "review fixes paused · ctrl+r retry".into()
            } else {
                review.label()
            })
            .fg(KEY_HINT),
        ];
        if !details
            && !stale
            && review.status == "blocked"
            && let Some(report) = &review.report
        {
            lines.push(Line::from(report.summary.clone()).fg(Color::Red));
        }
        if review.report.is_some() {
            lines.push(Line::from(vec![
                "ctrl+g i".fg(ACCENT),
                " View review findings".fg(KEY_HINT),
            ]));
        }
        if details && let Some(report) = &review.report {
            lines.push(Line::from(report.summary.clone()));
            for finding in &report.findings {
                lines.push(Line::default());
                lines.push(Line::from(format!("{} · {}", finding.priority, finding.title)).bold());
                lines.push(
                    Line::from(format!(
                        "{}:{} · {}",
                        finding.file, finding.line, finding.owner
                    ))
                    .fg(KEY_HINT),
                );
                lines.push(Line::from(finding.evidence.clone()));
                lines.push(Line::from(format!("Fix: {}", finding.fix)));
            }
        }
        blocks.push(ConversationBlock {
            text: lines.into(),
            user: false,
            plan: false,
        });
    }
    for message in &code_mod.coordination {
        if let Some(id) = task_filter {
            let relevant = message.task == Some(id)
                || message.active
                    && code_mod.execution.as_ref().is_some_and(|e| {
                        e.tasks
                            .iter()
                            .any(|run| run.id == id && message.to_task == run.task_id)
                    });
            if !relevant {
                continue;
            }
        }
        let question = code_mod.needs_answer(message);
        if !question && !history {
            continue;
        }
        let state = if message.answered {
            "answered"
        } else if message.acknowledged.is_some() {
            "acknowledged"
        } else if message.delivered.is_some() {
            "delivered"
        } else if !message.active {
            "previous round"
        } else {
            "waiting"
        };
        let mut lines = vec![
            Line::from(format!(
                "{} {} · {}{} → {} · #{}",
                if question { "?" } else { "↳" },
                message.kind,
                message.from_task,
                message
                    .sender
                    .map_or(String::new(), |id| format!(" · w{id}")),
                message.to_task,
                message.id
            ))
            .fg(if question { ACCENT } else { KEY_HINT })
            .bold(),
        ];
        lines.extend(message.body.lines().map(|line| Line::from(line.to_owned())));
        if question {
            let first = code_mod.question().unwrap().id;
            lines.push(
                Line::from(if message.id == first {
                    "↵ answer in the composer".to_owned()
                } else {
                    format!("waiting · answer #{first} first")
                })
                .fg(ACCENT),
            );
        } else {
            lines.push(Line::from(state).fg(MUTED));
        }
        blocks.push(ConversationBlock {
            text: lines.into(),
            user: false,
            plan: false,
        });
    }
    blocks
}

fn draw_conversation(frame: &mut Frame, app: &mut App, area: Rect, elapsed: Option<Duration>) {
    app.page_size = area.height.max(1);
    if area.is_empty() {
        return;
    }
    let activity = app
        .current_worker()
        .filter(|worker| {
            worker.role != Role::Planner
                && matches!(
                    worker.status,
                    Status::Starting | Status::Running | Status::Checking | Status::Stopping
                )
        })
        .map(|_| activity_glyph(elapsed));
    let blocks = app.current_mod().map_or_else(Vec::new, |m| {
        conversation_blocks(
            m,
            app.plan_details,
            false,
            area.width,
            activity,
            &app.network_requests,
            None,
        )
    });
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
    draw_message_rows(frame, rows, area, scroll);
    app.history_offset = history_offset;
    app.focus_plan = false;
}

fn draw_message_rows(
    frame: &mut Frame,
    rows: Vec<(usize, usize, bool, Paragraph<'_>)>,
    area: Rect,
    scroll: usize,
) {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hanging_wrap_preserves_unicode_and_long_tokens() {
        let source = "     café 界界 src/averylongfilename_without_spaces.rs";
        let rows = wrap_line(Line::from(source).fg(ACCENT), 18, 5);
        assert!(rows.len() > 1);
        assert!(rows.iter().all(|row| row.width() <= 18));
        assert!(rows.iter().all(|row| row.to_string().starts_with("     ")));
        let compact = |text: String| {
            text.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        };
        assert_eq!(
            compact(rows.iter().map(ToString::to_string).collect()),
            compact(source.into())
        );
        assert!(
            rows.iter()
                .flat_map(|row| row.styled_graphemes(Style::new()))
                .filter(|g| g.symbol != " ")
                .all(|g| g.style.fg == Some(ACCENT))
        );
    }
}
