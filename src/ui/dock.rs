use super::*;
use crate::app::controls::Control;

struct Part {
    text: Line<'static>,
    control: Option<Control>,
}

impl Part {
    fn button(text: Line<'static>, control: Control) -> Self {
        let mut padded = vec![Span::raw(" ")];
        padded.extend(text.spans);
        padded.push(Span::raw(" "));
        Self {
            text: Line::from(padded).bg(CONTROL),
            control: Some(control),
        }
    }
}

#[derive(Default)]
pub(super) struct Rows {
    lines: Vec<Line<'static>>,
    controls: Vec<(u16, u16, u16, Control)>,
}

impl Rows {
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    fn pair(&mut self, left: Vec<Part>, right: Vec<Part>, width: u16) {
        let size = |parts: &[Part]| {
            parts.iter().map(|p| p.text.width()).sum::<usize>() + parts.len().saturating_sub(1) * 2
        };
        if size(&left) + size(&right) + 3 <= width as usize {
            let mut row = Line::default();
            let right_start = width as usize - size(&right);
            for (parts, start) in [(left, 0), (right, right_start)] {
                for (index, part) in parts.into_iter().enumerate() {
                    let x = if index == 0 { start } else { row.width() + 2 };
                    row.spans
                        .push(Span::raw(" ".repeat(x.saturating_sub(row.width()))));
                    self.part(&mut row, part, x as u16);
                }
            }
            self.lines.push(row);
            return;
        }
        for (parts, align_right) in [(left, false), (right, true)] {
            let mut row = Line::default();
            for part in parts {
                let size = part.text.width().min(width as usize);
                if row.width() + size + 2 > width as usize && row.width() > 0 {
                    self.lines.push(row);
                    row = Line::default();
                }
                let x = if align_right && row.width() == 0 {
                    width as usize - size
                } else if row.width() == 0 {
                    0
                } else {
                    row.width() + 2
                };
                row.spans
                    .push(Span::raw(" ".repeat(x.saturating_sub(row.width()))));
                self.part(&mut row, part, x as u16);
            }
            if row.width() > 0 {
                self.lines.push(row);
            }
        }
    }

    fn part(&mut self, row: &mut Line<'static>, part: Part, x: u16) {
        if let Some(control) = part.control {
            self.controls.push((
                self.lines.len() as u16,
                x,
                part.text.width() as u16,
                control,
            ));
        }
        let style = part.text.style;
        row.spans
            .extend(part.text.spans.into_iter().map(|mut span| {
                span.style = style.patch(span.style);
                span
            }));
    }

    pub fn draw(self, frame: &mut Frame, app: &mut App, area: Rect) {
        for (row, x, width, control) in self.controls {
            if row < area.height && x < area.width {
                app.controls.add(
                    Rect::new(area.x + x, area.y + row, width.min(area.width - x), 1),
                    control,
                );
            }
        }
        frame.render_widget(Paragraph::new(self.lines), area);
    }
}

pub(super) fn header(
    dock: &ActionDock,
    width: u16,
    elapsed: Option<Duration>,
    menu_open: bool,
) -> Rows {
    let (glyph, color) = match dock.tone {
        Tone::Quiet => ("◇", KEY_HINT),
        Tone::Busy => (activity_glyph(elapsed), ACCENT),
        Tone::Attention => ("!", Color::Red),
        Tone::Ready => ("✓", ACCENT),
    };
    let mut rows = Rows::default();
    let primary = (!menu_open)
        .then_some(dock.primary)
        .flatten()
        .and_then(|action| dock.actions.iter().find(|i| i.action == action));
    rows.pair(
        vec![Part {
            text: Line::from(fit_name(&format!("{glyph} {}", dock.status), width))
                .fg(color)
                .bold(),
            control: None,
        }],
        primary
            .map(|item| {
                Part::button(
                    action_control(item, width, false).bold(),
                    Control::Action(item.action),
                )
            })
            .into_iter()
            .collect(),
        width,
    );
    if menu_open {
        return rows;
    }
    for text in [&dock.detail, &dock.evidence] {
        if !text.is_empty() {
            rows.lines.extend(
                wrap_line(Line::from(text.clone()).fg(KEY_HINT), width, 0)
                    .into_iter()
                    .take(2),
            );
        }
    }
    if let Some(error) = &dock.error {
        let mut lines: Vec<_> = indented_text(error, width, 0)
            .into_iter()
            .map(|line| line.fg(Color::Red))
            .collect();
        if lines.len() > 2 {
            lines[1] = Line::from(format!(
                "{}…",
                fit_name(&lines[1].to_string(), width.saturating_sub(1))
            ))
            .fg(Color::Red);
        }
        rows.lines.extend(lines.into_iter().take(2));
    }
    if let Some(sync) = dock.sync {
        rows.lines.extend(wrap_line(
            Line::from(format!("! {sync}")).fg(Color::Yellow),
            width,
            0,
        ));
        rows.pair(
            Vec::new(),
            vec![
                Part::button(
                    Line::from("ctrl+u Retry sync").fg(ACCENT),
                    Control::Action(Action::Update),
                ),
                Part::button(
                    Line::from("ctrl+g s Sync details").fg(KEY_HINT),
                    Control::Action(Action::SyncDetails),
                ),
            ],
            width,
        );
    }
    rows
}

pub(super) fn hints(app: &App, width: u16, menu_open: bool) -> Rows {
    let mut rows = Rows::default();
    if menu_open {
        rows.lines.push(Line::from("esc Back").fg(KEY_HINT));
        return rows;
    }
    let new_mod = matches!(app.composer_view(), View::NewMod);
    let ctrl = if width < 60 { "^" } else { "ctrl+" };
    let button = |key: &str, label: &str, control| {
        Part::button(
            Line::from(vec![
                format!("{key} ").fg(ACCENT),
                label.to_owned().fg(KEY_HINT),
            ]),
            control,
        )
    };
    let mut actions = Vec::new();
    let dock = app.action_dock();
    if new_mod {
        actions.push(button("F2", "Format", Control::Format));
    } else {
        if dock.primary != Some(Action::Stop)
            && dock.actions.iter().any(|i| i.action == Action::Stop)
        {
            actions.push(button(
                &format!("{ctrl}r"),
                "Pause",
                Control::Action(Action::Stop),
            ));
        }
        actions.push(button(&format!("{ctrl}t"), "Inspect", Control::Inspect));
    }
    actions.push(button(&format!("{ctrl}g"), "More", Control::More));
    let mut input = Vec::new();
    if new_mod || !app.read_only() {
        let send = if new_mod {
            "Create codemod".into()
        } else if let Some(question) = app.current_mod().and_then(|m| m.question()) {
            format!("Answer #{}", question.id)
        } else if app.published() || app.version_ready() {
            "Request edits".into()
        } else {
            "Queue instruction".into()
        };
        input.push(button("↵", &send, Control::Submit));
        input.push(button(&format!("{ctrl}j"), "Newline", Control::Newline));
    }
    rows.pair(input, actions, width);
    rows
}
