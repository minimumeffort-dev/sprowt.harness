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
const MUTED: Color = Color::DarkGray;
const CONTROL: Color = Color::Rgb(51, 59, 50);
const SELECTED: Color = Color::Rgb(62, 73, 55);
const USER_BACKGROUND: Color = Color::Rgb(43, 49, 43);
const KEY_HINT: Color = Color::Rgb(161, 170, 160);
const MOD_GLYPH: &str = "◇";
const DIALOG_WIDTH: u16 = 100;

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
    let area = frame.area().inner(Margin::new(2, 1));
    let composer_view = app.composer_view();
    let show_dock = matches!(composer_view, View::Chat | View::NewMod);
    let dock = app.action_dock();
    let dock_lines = dock_lines(&dock, area.width, elapsed);
    let mod_height = u16::from(!matches!(
        composer_view,
        View::NewMod | View::ProjectSetup(false, _)
    ));
    if area.width < 32
        || area.height
            < 11 + mod_height
                + if show_dock {
                    dock_lines.len() as u16
                } else {
                    0
                }
    {
        frame.render_widget(
            Paragraph::new("Make the terminal a little larger.\nCtrl+C to quit.")
                .wrap(Wrap { trim: false }),
            area,
        );
        return Rect::default();
    }

    let gap = u16::from(area.height >= 26 + 2 * mod_height);
    let read_only = matches!(composer_view, View::Chat) && app.read_only();
    let [header, _, mod_row, _, content, _, dock_area, input, footer] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(gap),
        Constraint::Length(mod_height),
        Constraint::Length(gap * mod_height),
        Constraint::Min(1),
        Constraint::Length(gap),
        Constraint::Length(if show_dock {
            dock_lines.len() as u16
        } else {
            0
        }),
        Constraint::Length(if read_only { 0 } else { 5 }),
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
    let status = app.current_worker().map_or(String::new(), |worker| {
        worker.label(worker.busy().then(|| activity_glyph(elapsed)))
    });
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec!["sprowt".fg(ACCENT).bold(), " harness".bold()]),
            Line::from(fit_name(&project, heading.width)).fg(MUTED),
            Line::from(fit_name(&status, heading.width)).fg(KEY_HINT),
            Line::from(if !show_dock {
                app.worker_error()
                    .map_or(String::new(), |error| fit_name(error, heading.width))
            } else {
                String::new()
            })
            .fg(Color::Red),
        ]),
        heading,
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
    let mut can_scroll = false;
    if matches!(composer_view, View::Chat | View::EditQueue(_)) {
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
        can_scroll = draw_conversation(frame, app, conversation, elapsed);
        draw_queue_preview(frame, app, queue);
        draw_steering_preview(frame, app, steering);
    }
    let dialog_area = Rect {
        y: mod_row.bottom() + gap,
        width: area.width.min(DIALOG_WIDTH),
        height: area.bottom().saturating_sub(mod_row.bottom() + gap),
        ..area
    };
    if let View::Actions(selected) = app.view {
        if !read_only {
            frame.render_widget(&app.input, input);
        }
        draw_actions(frame, &dock, selected, content);
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
            Constraint::Length(rows as u16),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .areas(body);
        frame.render_widget(choices, choices_area);
        frame.render_widget(&app.input, field);
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
        frame.render_widget(text, body);
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
    } else if let View::History(scroll) = app.view {
        draw_history(
            frame,
            app,
            scroll,
            Rect {
                width: area.width,
                ..dialog_area
            },
        );
    } else if matches!(app.view, View::Publish) {
        let count = app.review.as_ref().map_or(0, |review| review.count());
        let text = Paragraph::new(format!(
            "Publish {count} changed files {}?",
            if app.git_state().is_some_and(|s| s.pr.is_some()) {
                "to the existing PR"
            } else {
                "as a PR"
            }
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
        frame.render_widget(text, body);
    } else {
        if !read_only {
            let title = app
                .current_mod()
                .and_then(|m| m.question())
                .map_or("message".to_owned(), |q| format!("answer #{}", q.id));
            if matches!(app.view, View::Chat) {
                app.input.set_block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .border_style(Style::new().fg(ACCENT))
                        .padding(Padding::horizontal(1))
                        .title(format!(" {title} ")),
                );
                app.input.set_placeholder_text(if title == "message" {
                    "Describe a feature, a fix, or an idea..."
                } else {
                    "Answer the highlighted question..."
                });
            }
            frame.render_widget(&app.input, input);
        }
        let shortcuts = match composer_view {
            View::NewMod => format!(
                "↵ create + build{}   esc {}",
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
    if show_dock {
        frame.render_widget(Paragraph::new(dock_lines), dock_area);
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
    let target = app
        .steer_target
        .map_or_else(|| "target: all".into(), |id| format!("target: w{id}"));
    if !app.running_workers().is_empty() {
        hints.push(("t", &target));
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

fn draw_mod_picker(frame: &mut Frame, app: &App, index: usize, area: Rect) {
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
            &mut ListState::default().with_selected((index < count).then_some(index)),
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

fn draw_delete_mod(frame: &mut Frame, app: &App, index: usize, area: Rect) {
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
    frame.render_widget(body, content);
}

fn draw_close_mod(frame: &mut Frame, app: &App, index: usize, area: Rect) {
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
    frame.render_widget(body, content);
}

fn draw_review(frame: &mut Frame, app: &mut App, scroll: u16, area: Rect) {
    let Some(review) = &app.review else {
        return;
    };
    let [panel, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(MUTED))
        .padding(Padding::horizontal(1))
        .title(
            Line::from(format!(" diff · {} files ", review.count()))
                .fg(KEY_HINT)
                .bold(),
        );
    let inner = block.inner(panel);
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
    frame.render_widget(
        Line::from(if app.can_publish() {
            "↑↓ scroll  fn+↑/↓ page  r ask agent to review  p publish PR  esc back"
        } else {
            "↑↓ scroll  fn+↑/↓ page  esc back"
        })
        .fg(KEY_HINT),
        footer,
    );
}

fn draw_history(frame: &mut Frame, app: &mut App, scroll: u16, area: Rect) {
    let Some(code_mod) = app.current_mod() else {
        return;
    };
    let [panel, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(MUTED))
        .padding(Padding::horizontal(1))
        .title(Line::from(" worker history ").fg(KEY_HINT).bold());
    let inner = block.inner(panel);
    let mut rows = Vec::new();
    let mut total: usize = 0;
    for item in conversation_blocks(code_mod, false, true, inner.width, None, false, &[]) {
        let paragraph = Paragraph::new(item.text).wrap(Wrap { trim: false });
        let height =
            paragraph.line_count(inner.width.saturating_sub(if item.user { 2 } else { 0 }));
        rows.push((total, height, item.user, paragraph));
        total += height + 1;
    }
    let max_scroll = total
        .saturating_sub(1)
        .saturating_sub(inner.height as usize)
        .min(u16::MAX as usize) as u16;
    let scroll = scroll.min(max_scroll);
    frame.render_widget(block, panel);
    draw_message_rows(frame, rows, inner, scroll as usize);
    app.page_size = inner.height.max(1);
    app.view = View::History(scroll);
    frame.render_widget(
        Line::from(if cfg!(target_os = "macos") {
            "↑↓ scroll  fn+↑/↓ page  esc back"
        } else {
            "↑↓ scroll  pgup/pgdn page  esc back"
        })
        .fg(KEY_HINT),
        footer,
    );
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
    frame.render_widget(body.scroll((scroll, 0)), content);
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
        height: (rows.clamp(1, 12) as u16 + action_height + 3 + hints_height).min(area.height),
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
    let mut hints = if app.read_only() {
        String::new()
    } else if app.current_mod().and_then(|m| m.question()).is_some() {
        "↵ answer".to_owned()
    } else if app.published() || app.version_ready() {
        "↵ request edits".to_owned()
    } else {
        "↵ queue".to_owned()
    };
    let ctrl = if width < 60 { "^" } else { "ctrl+" };
    let mut options = Vec::new();
    if !app.read_only() {
        options.push(format!("{ctrl}j newline"));
    }
    if can_scroll {
        options.push(if cfg!(target_os = "macos") {
            "fn+↑/↓ scroll".into()
        } else {
            "pgup/pgdn scroll".into()
        });
    }
    for option in options {
        let candidate = format!("{hints}  {option}");
        if Span::raw(&candidate).width() + "  esc quit".len() <= width as usize {
            hints = candidate;
        }
    }
    format!("{hints}  esc quit")
}

fn action_control(item: &ActionItem, width: u16) -> Line<'static> {
    let mut spans = Vec::new();
    if let Some(key) = item.action.shortcut() {
        spans.push(format!("{}{key} ", if width < 60 { "^" } else { "ctrl+" }).fg(ACCENT));
    }
    spans.push(item.label.clone().fg(KEY_HINT));
    Line::from(spans)
}

fn dock_lines(dock: &ActionDock, width: u16, elapsed: Option<Duration>) -> Vec<Line<'static>> {
    let (glyph, color) = match dock.tone {
        Tone::Quiet => ("◇", KEY_HINT),
        Tone::Busy => (activity_glyph(elapsed), KEY_HINT),
        Tone::Attention => ("!", Color::Red),
        Tone::Ready => ("✓", ACCENT),
    };
    let mut lines =
        vec![Line::from(fit_name(&format!("{glyph} {}", dock.status), width)).fg(color)];
    if let Some(error) = &dock.error {
        lines.push(Line::from(fit_name(error, width)).fg(Color::Red));
    }
    let mut next = Line::from("Next · ".fg(KEY_HINT));
    if let Some(primary) = dock
        .primary
        .and_then(|id| dock.actions.iter().find(|a| a.action == id))
    {
        next.spans.extend(
            action_control(primary, width)
                .spans
                .into_iter()
                .map(|s| s.bold().bg(CONTROL)),
        );
    } else if let Some(guidance) = dock.guidance {
        next.spans.push(guidance.to_owned().fg(KEY_HINT));
    } else {
        next = Line::default();
    }
    // Secondary actions fit on the same row; the menu always has its own space.
    for action in [
        Action::Queue,
        Action::Diff,
        Action::Publish,
        Action::Stop,
        Action::Retry,
        Action::RetryGit,
        Action::Run,
        Action::Details,
    ] {
        if dock.primary == Some(action) {
            continue;
        }
        if let Some(item) = dock.actions.iter().find(|a| a.action == action) {
            let control = action_control(item, width);
            if next.width() + control.width() + 2 <= width as usize {
                next.spans.push(Span::raw("  "));
                next.spans.extend(control.spans);
            }
        }
    }
    let all = Line::from(vec![
        format!("{}g ", if width < 60 { "^" } else { "ctrl+" }).fg(ACCENT),
        "All actions".fg(KEY_HINT),
    ]);
    if next.width() + all.width() + 2 <= width as usize {
        next.spans.push(Span::raw("  "));
        next.spans.extend(all.spans);
        lines.push(next);
    } else {
        if next.width() > 0 {
            lines.push(next);
        }
        lines.push(all);
    }
    lines
}

fn draw_actions(frame: &mut Frame, dock: &ActionDock, selected: Action, area: Rect) {
    frame.render_widget(ratatui::widgets::Clear, area);
    let (body, _) = draw_dialog(
        frame,
        area,
        "all actions",
        dock.actions.len(),
        false,
        &[("↑↓", "select"), ("↵", "choose"), ("esc", "back")],
    );
    let items = dock.actions.iter().map(|item| {
        let mut line = action_control(item, body.width);
        if dock.primary == Some(item.action) {
            line.spans.insert(0, "› ".fg(ACCENT));
        } else {
            line.spans.insert(0, Span::raw("  "));
        }
        line
    });
    let index = dock.actions.iter().position(|a| a.action == selected);
    frame.render_stateful_widget(
        List::new(items).highlight_style(dialog_selection()),
        body,
        &mut ListState::default().with_selected(index),
    );
}

fn draw_failure(frame: &mut Frame, app: &mut App, scroll: u16, area: Rect) {
    let [panel, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    app.page_size = panel.height.saturating_sub(2).max(1);
    let body = Paragraph::new(app.action_error().unwrap_or("No current error."))
        .wrap(Wrap { trim: false })
        .block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(MUTED))
                .padding(Padding::horizontal(1))
                .title(" error details "),
        );
    let maximum = body
        .line_count(panel.width.saturating_sub(4))
        .saturating_sub(panel.height.saturating_sub(2) as usize)
        .min(u16::MAX as usize) as u16;
    frame.render_widget(body.scroll((scroll.min(maximum), 0)), panel);
    app.view = View::Failure(scroll.min(maximum));
    frame.render_widget(Line::from("↑↓ scroll  esc back").fg(KEY_HINT), footer);
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
    let capacity = width.saturating_sub(7).max(1) as usize;
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
        .map(|line| Line::from(vec!["     │ ".fg(MUTED), line.fg(KEY_HINT)]))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn plan_lines(
    plan: &Plan,
    planning: &Planning,
    execution: Option<&Execution>,
    details: bool,
    width: u16,
    activity: Option<&'static str>,
    published: bool,
    network: &[crate::network::Access],
) -> Vec<Line<'static>> {
    let completed = execution
        .is_some_and(|e| e.complete() && matches!(e.status.as_str(), "review" | "applied"));
    let compact = completed && !details;
    let mut lines = vec![
        Line::from(if planning.source.starts_with("upstream:") {
            "▤ integration plan"
        } else {
            "▤ codex · planner"
        })
        .fg(ACCENT)
        .bold(),
        Line::from(if let Some(execution) = execution {
            format!(
                "{} · {}/{} tasks done",
                match execution.status.as_str() {
                    "review" | "applied" if published => "PR published",
                    "review" => "changes ready",
                    "applied" => "saved version",
                    "blocked" if execution.tasks.iter().any(|run| run.status == "waiting") =>
                        "waiting for reply",
                    "blocked" => "execution paused",
                    "verifying" => "final checks",
                    "repairing"
                        if execution
                            .tasks
                            .iter()
                            .any(|run| run.status == "repair_paused") =>
                        "repair paused",
                    "repairing" => "repair requested",
                    _ if execution
                        .tasks
                        .iter()
                        .any(|run| run.repair.is_some() && run.status != "done") =>
                        "repairing",
                    _ => "execution",
                },
                execution
                    .tasks
                    .iter()
                    .filter(|run| run.status == "done")
                    .count(),
                execution.tasks.len()
            )
        } else {
            format!(
                "plan ready · {} {}",
                plan.tasks.len(),
                if plan.tasks.len() == 1 {
                    "task"
                } else {
                    "tasks"
                }
            )
        })
        .fg(ACCENT)
        .bold(),
    ];
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
                lines.extend(notes.iter().map(|note| Line::from(format!("   · {note}"))));
            }
        }
    }
    for (index, task) in plan.tasks.iter().enumerate() {
        let run = execution
            .and_then(|execution| execution.tasks.iter().find(|run| run.task_id == task.id));
        let marker = run.map_or("", |run| match run.status.as_str() {
            "done" => "✓",
            "running" | "sending" | "checking" if activity.is_some() => activity.unwrap(),
            "running" | "sending" => "●",
            "checking" => "◌",
            "blocked" | "paused" | "repair_paused" => "!",
            "waiting" => "?",
            "repair_wait" => "↺",
            _ => "○",
        });
        let prefix = if marker.is_empty() {
            format!("{}. ", index + 1)
        } else {
            format!("{marker} {}. ", index + 1)
        };
        if !compact {
            lines.push(Line::default());
        }
        let title = if compact {
            fit_name(
                &task.title,
                width.saturating_sub(Span::raw(&prefix).width() as u16),
            )
        } else {
            task.title.clone()
        };
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
            lines.push(Line::from(format!("   {}", task.outcome)));
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
                    "pending" => "   ! Network access needed",
                    "denied" => "   ! Network access denied · task paused",
                    _ => "   ◌ Access approved · reconnecting worker",
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
            if let Some(provider) = run.and_then(|run| run.provider.as_deref()) {
                let reason = &run.unwrap().assignment_reason;
                lines.push(
                    Line::from(format!(
                        "   worker · {provider}{}",
                        if reason.is_empty() {
                            String::new()
                        } else {
                            format!(" · {reason}")
                        }
                    ))
                    .fg(KEY_HINT),
                );
            } else if task.worker == "auto" {
                lines.push(Line::from("   worker · assigned when ready").fg(KEY_HINT));
            } else {
                lines.push(
                    Line::from(format!(
                        "   worker · {}{}",
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
                lines.push(Line::from(format!("   repair check · {}", repair.check)).fg(ACCENT));
                lines.extend(command_lines(&repair.command, width));
                lines.extend(
                    repair
                        .evidence
                        .lines()
                        .take(6)
                        .map(|line| Line::from(format!("     {line}")).fg(KEY_HINT)),
                );
            }
            if let Some(selection) = run.and_then(|run| run.selection.as_ref()) {
                lines.push(
                    Line::from(format!(
                        "   model · {} · {}",
                        selection.model, selection.effort
                    ))
                    .fg(KEY_HINT),
                );
                lines.push(Line::from(format!("   {}", selection.display_reason())).fg(MUTED));
            }
            if !task.files.is_empty() {
                lines
                    .push(Line::from(format!("   files · {}", task.files.join(", "))).fg(KEY_HINT));
            }
            for peer in plan.peers(task) {
                let number = plan.tasks.iter().position(|t| t.id == peer.task).unwrap() + 1;
                lines.push(
                    Line::from(format!(
                        "   with task {number} · {}",
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
                let failed = run
                    .checks
                    .iter()
                    .filter(|check| check.exit_code.is_some_and(|code| code != 0))
                    .count();
                lines.push(
                    Line::from(format!(
                        "   verification · {passed}/{total} passed · {failed} failed · {} not run",
                        total - passed - failed
                    ))
                    .fg(if passed == total { ACCENT } else { Color::Red }),
                );
            }
            lines.push(Line::from("   checks").fg(KEY_HINT));
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
                    "   {} {check}{}",
                    if results.is_empty() {
                        "·"
                    } else if results.iter().all(|result| result.exit_code == Some(0)) {
                        "✓"
                    } else if results
                        .iter()
                        .any(|result| result.exit_code.is_some_and(|code| code != 0))
                    {
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
                    lines.extend(command_lines(&result.command, width));
                    if result.exit_code != Some(0) {
                        lines.extend(
                            result
                                .evidence()
                                .lines()
                                .map(|line| Line::from(format!("     {line}")).fg(Color::Red)),
                        );
                    }
                }
            }
            if let Some(run) = run
                && !run.summary.is_empty()
            {
                lines.push(Line::from(format!("   worker report · {}", run.summary)).fg(MUTED));
            }
            if let Some(run) = run
                && run.status != "done"
                && run.checks.is_empty()
                && !run.verification_feedback.is_empty()
            {
                lines.push(Line::from("   previous verification · recovery context").fg(KEY_HINT));
                for check in run
                    .verification_feedback
                    .iter()
                    .filter(|check| check.exit_code != Some(0))
                {
                    lines.push(Line::from(format!("   ! {}", check.check)).fg(Color::Red));
                    lines.extend(
                        check
                            .evidence()
                            .lines()
                            .map(|line| Line::from(format!("     {line}")).fg(Color::Red)),
                    );
                }
            }
        }
    }
    if details {
        if let Some(execution) = execution
            && !execution.checks.is_empty()
        {
            if !completed {
                lines.push(
                    Line::from(format!(
                        "final checks · {} / {} passed",
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
            for check in execution
                .checks
                .iter()
                .filter(|check| check.exit_code != Some(0))
            {
                lines.push(Line::from(format!("! {}", check.check)).fg(Color::Red));
                lines.extend(
                    check
                        .evidence()
                        .lines()
                        .map(|line| Line::from(line.to_owned()).fg(Color::Red)),
                );
            }
        }
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
    published: bool,
    network: &[crate::network::Access],
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
                        published,
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

fn draw_conversation(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    elapsed: Option<Duration>,
) -> bool {
    app.page_size = area.height.max(1);
    if area.is_empty() {
        return false;
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
            app.published(),
            &app.network_requests,
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
    max_scroll > 0
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
