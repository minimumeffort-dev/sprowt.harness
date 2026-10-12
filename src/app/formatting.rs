use super::{App, View};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui_textarea::{CursorMove, TextArea};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Heading,
    Bold,
    Italic,
    BulletedList,
    Checklist,
    InlineCode,
    CodeBlock,
}

impl Format {
    pub const ALL: [Self; 7] = [
        Self::Heading,
        Self::Bold,
        Self::Italic,
        Self::BulletedList,
        Self::Checklist,
        Self::InlineCode,
        Self::CodeBlock,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Heading => "Heading",
            Self::Bold => "Bold",
            Self::Italic => "Italic",
            Self::BulletedList => "Bulleted list",
            Self::Checklist => "Checklist",
            Self::InlineCode => "Inline code",
            Self::CodeBlock => "Code block",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Self::Heading => "#",
            Self::Bold => "B",
            Self::Italic => "I",
            Self::BulletedList => "•",
            Self::Checklist => "☐",
            Self::InlineCode => "`",
            Self::CodeBlock => "{}",
        }
    }
}

#[derive(Default)]
pub struct Menu {
    pub selected: usize,
    slash: Option<(usize, usize)>,
}

impl App {
    pub(super) fn open_formatting(&mut self) {
        if self.engine.view == View::NewMod {
            self.formatting_menu = Some(Menu::default());
        }
    }

    pub(super) fn choose_format(&mut self, format: Format) {
        if self.engine.view != View::NewMod {
            return;
        }
        let Some(menu) = self.formatting_menu.take() else {
            return;
        };
        if let Some(position) = menu.slash {
            move_to(&mut self.input, position);
            self.input.delete_next_char();
        }
        apply(&mut self.input, format);
    }

    pub(super) fn formatting_key(&mut self, key: KeyEvent) -> bool {
        if self.engine.view != View::NewMod {
            return false;
        }
        if key.code == KeyCode::F(2) && key.modifiers.is_empty() {
            if key.kind == KeyEventKind::Press {
                if self.formatting_menu.is_some() {
                    self.formatting_menu = None;
                } else {
                    self.open_formatting();
                }
            }
            return true;
        }
        if let Some(menu) = &mut self.formatting_menu {
            match key.code {
                KeyCode::Up if key.modifiers.is_empty() => {
                    menu.selected = menu.selected.saturating_sub(1);
                    return true;
                }
                KeyCode::Down if key.modifiers.is_empty() => {
                    menu.selected = (menu.selected + 1).min(Format::ALL.len() - 1);
                    return true;
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    if key.kind == KeyEventKind::Press {
                        let format = Format::ALL[menu.selected];
                        self.choose_format(format);
                    }
                    return true;
                }
                KeyCode::Esc => {
                    self.formatting_menu = None;
                    return true;
                }
                _ => self.formatting_menu = None,
            }
        }
        if matches!(key.code, KeyCode::Char('z' | 'Z'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                self.input.redo();
            } else {
                self.input.undo();
            }
            return true;
        }
        if key.code == KeyCode::Char('/')
            && key.modifiers.is_empty()
            && self.input.selection_range().is_none()
            && self.input.lines()[self.input.cursor().0].trim().is_empty()
            && !inside_fence(self.input.lines(), self.input.cursor().0)
        {
            let cursor = self.input.cursor();
            self.input.insert_char('/');
            self.formatting_menu = Some(Menu {
                selected: 0,
                slash: Some((cursor.0, cursor.1)),
            });
            return true;
        }
        false
    }
}

fn apply(input: &mut TextArea<'_>, format: Format) {
    let cursor = input.cursor();
    let selection = input.selection_range();
    let (start, end) = selection.unwrap_or(((cursor.0, cursor.1), (cursor.0, cursor.1)));
    match format {
        Format::Heading | Format::BulletedList | Format::Checklist => {
            let prefix = match format {
                Format::Heading => "# ",
                Format::BulletedList => "- ",
                _ => "- [ ] ",
            };
            let last = last_selected_line(start, end);
            for row in (start.0..=last).rev() {
                move_to(input, (row, 0));
                input.insert_str(prefix);
            }
            let shifted = |(row, col)| (row, col + usize::from(row <= last) * prefix.len());
            restore_selection(input, shifted(start), shifted(end), selection.is_some());
        }
        Format::Bold => emphasis(input, start, end, "**"),
        Format::Italic => emphasis(input, start, end, "*"),
        Format::InlineCode => {
            let text = selected_text(input, start, end);
            let fence = "`".repeat(longest_backticks(&text) + 1);
            let padding = if text.starts_with('`')
                || text.ends_with('`')
                || !text.trim().is_empty() && (text.starts_with(' ') || text.ends_with(' '))
            {
                " "
            } else {
                ""
            };
            wrap(
                input,
                start,
                end,
                &format!("{fence}{padding}"),
                &format!("{padding}{fence}"),
            );
        }
        Format::CodeBlock => {
            let start = (start.0, 0);
            let last = last_selected_line(start, end);
            let end = (last, input.lines()[last].chars().count());
            let fence =
                "`".repeat((longest_backticks(&selected_text(input, start, end)) + 1).max(3));
            wrap(
                input,
                start,
                end,
                &format!("{fence}\n"),
                &format!("\n{fence}"),
            );
        }
    }
}

fn last_selected_line(start: (usize, usize), end: (usize, usize)) -> usize {
    if end.0 > start.0 && end.1 == 0 {
        end.0 - 1
    } else {
        end.0
    }
}

fn emphasis(input: &mut TextArea<'_>, start: (usize, usize), end: (usize, usize), marker: &str) {
    if start == end {
        wrap(input, start, end, marker, marker);
        return;
    }
    let mut spans = Vec::new();
    let mut position = start;
    for line in selected_text(input, start, end).split_inclusive('\n') {
        if !line.trim().is_empty() {
            let leading = &line[..line.len() - line.trim_start().len()];
            spans.push((
                advance(position, leading),
                advance(position, line.trim_end()),
            ));
        }
        position = advance(position, line);
    }
    if spans.is_empty() {
        return;
    }
    for &(first, last) in spans.iter().rev() {
        wrap(input, first, last, marker, marker);
    }
    let selected_start = (
        start.0,
        start.1 + usize::from(spans[0].0 == start) * marker.len(),
    );
    let inserted_before_end = spans
        .iter()
        .flat_map(|&(first, last)| [first, last])
        .filter(|position| position.0 == end.0 && *position < end)
        .count();
    restore_selection(
        input,
        selected_start,
        (end.0, end.1 + inserted_before_end * marker.len()),
        true,
    );
}

fn wrap(
    input: &mut TextArea<'_>,
    start: (usize, usize),
    end: (usize, usize),
    prefix: &str,
    suffix: &str,
) {
    let body = selected_text(input, start, end);
    move_to(input, end);
    input.insert_str(suffix);
    move_to(input, start);
    input.insert_str(prefix);
    let start = advance(start, prefix);
    restore_selection(input, start, advance(start, &body), !body.is_empty());
}

fn selected_text(input: &TextArea<'_>, start: (usize, usize), end: (usize, usize)) -> String {
    (start.0..=end.0)
        .map(|row| {
            let from = if row == start.0 { start.1 } else { 0 };
            let to = if row == end.0 {
                end.1
            } else {
                input.lines()[row].chars().count()
            };
            input.lines()[row]
                .chars()
                .skip(from)
                .take(to - from)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn longest_backticks(text: &str) -> usize {
    text.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

fn advance(mut position: (usize, usize), text: &str) -> (usize, usize) {
    for character in text.chars() {
        if character == '\n' {
            position = (position.0 + 1, 0);
        } else {
            position.1 += 1;
        }
    }
    position
}

fn move_to(input: &mut TextArea<'_>, position: (usize, usize)) {
    input.cancel_selection();
    jump_to(input, position);
}

fn jump_to(input: &mut TextArea<'_>, position: (usize, usize)) {
    // Jump uses u16 coordinates; approach longer lines from the nearer end.
    input.move_cursor(CursorMove::Jump(
        position.0.min(u16::MAX as usize) as u16,
        position.1.min(u16::MAX as usize) as u16,
    ));
    let line_end = input.lines()[position.0].chars().count();
    if input.cursor().0 == position.0
        && position.1.saturating_sub(input.cursor().1) > line_end - position.1
    {
        input.move_cursor(CursorMove::End);
    }
    while (input.cursor().0, input.cursor().1) < position {
        input.move_cursor(CursorMove::Forward);
    }
    while (input.cursor().0, input.cursor().1) > position {
        input.move_cursor(CursorMove::Back);
    }
}

fn restore_selection(
    input: &mut TextArea<'_>,
    start: (usize, usize),
    end: (usize, usize),
    select: bool,
) {
    move_to(input, start);
    if select {
        input.start_selection();
        jump_to(input, end);
    }
}

fn inside_fence(lines: &[String], row: usize) -> bool {
    let mut open = None;
    for line in &lines[..row] {
        let trimmed = line.trim_start();
        let Some(marker @ ('`' | '~')) = trimmed.chars().next() else {
            continue;
        };
        let count = trimmed.chars().take_while(|c| *c == marker).count();
        if count < 3 {
            continue;
        }
        match open {
            Some((previous, length))
                if marker == previous && count >= length && trimmed[count..].trim().is_empty() =>
            {
                open = None;
            }
            None => open = Some((marker, count)),
            _ => {}
        }
    }
    open.is_some()
}
