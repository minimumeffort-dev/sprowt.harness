use super::*;

pub(super) fn lines(source: &str, width: u16) -> Vec<Line<'static>> {
    let source: Vec<_> = source.split('\n').collect();
    let mut lines = Vec::new();
    let mut index = 0;
    while index < source.len() {
        let line = source[index];
        let fence = line.chars().take_while(|c| *c == '`').count();
        if fence >= 3
            && !line[fence..].contains('`')
            && let Some(end) = (index + 1..source.len()).find(|i| {
                let closing = source[*i].trim();
                closing.len() >= fence && closing.chars().all(|c| c == '`')
            })
        {
            if !line[fence..].trim().is_empty() {
                lines.extend(wrap_line(
                    Line::from(line[fence..].trim().to_owned()).fg(MUTED),
                    width,
                    0,
                ));
            }
            lines.extend(code(&source[index + 1..end].join("\n"), width));
            index = end + 1;
            continue;
        }
        let hashes = line.chars().take_while(|c| *c == '#').count();
        let heading = (1..=6).contains(&hashes) && line[hashes..].starts_with(' ');
        let body = if heading { &line[hashes + 1..] } else { line };
        let bullet = ["- ", "* ", "+ "]
            .iter()
            .find_map(|prefix| body.strip_prefix(prefix));
        let ordered = body
            .split_once(". ")
            .filter(|(number, _)| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()));
        let (prefix, body) = if let Some(body) = bullet {
            ("• ".to_owned(), body)
        } else if let Some((number, body)) = ordered {
            (format!("{number}. "), body)
        } else {
            (String::new(), body)
        };
        let indent = Span::raw(&prefix).width();
        let mut spans = vec![Span::styled(prefix, Style::new().fg(MUTED))];
        spans.extend(inline(body));
        let line = Line::from(spans);
        lines.extend(wrap_line(
            if heading { line.bold() } else { line },
            width,
            indent,
        ));
        index += 1;
    }
    lines
}

// Render only paired inline code and emphasis markers; other syntax stays readable as written.
pub(super) fn inline(mut text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    while !text.is_empty() {
        let next = text.char_indices().find(|(index, c)| {
            (*c == '`' || *c == '*') && (*index == 0 || !text[..*index].ends_with('\\'))
        });
        let Some((start, marker)) = next else {
            spans.push(Span::raw(text.to_owned()));
            break;
        };
        spans.push(Span::raw(text[..start].to_owned()));
        text = &text[start..];
        let count = text.chars().take_while(|c| *c == marker).count();
        let supported = marker == '`' && count == 1 || marker == '*' && matches!(count, 1 | 2);
        let delimiter = &text[..count];
        let closing = supported
            .then(|| {
                text[count..]
                    .match_indices(delimiter)
                    .map(|(index, _)| index)
                    .find(|index| {
                        let prefix = &text[..count + index];
                        let suffix = &text[count + index + count..];
                        !prefix.ends_with('\\')
                            && !prefix.ends_with(marker)
                            && !suffix.starts_with(marker)
                    })
            })
            .flatten();
        if let Some(end) = closing.filter(|end| {
            let body = &text[count..count + end];
            !body.is_empty() && (marker == '`' || body.trim() == body)
        }) {
            let body = &text[count..count + end];
            if marker == '`' {
                spans.push(Span::styled(
                    body.to_owned(),
                    Style::new().fg(INK).bg(CONTROL),
                ));
            } else {
                spans.extend(inline(body).into_iter().map(|span| {
                    if count == 2 {
                        span.bold()
                    } else {
                        span.italic()
                    }
                }));
            }
            text = &text[count + end + count..];
        } else {
            spans.push(Span::raw(delimiter.to_owned()));
            text = &text[count..];
        }
    }
    spans
}

pub(super) fn code(source: &str, width: u16) -> Vec<Line<'static>> {
    let capacity = width.saturating_sub(2).max(1) as usize;
    let mut lines = Vec::new();
    for line in source.split('\n') {
        let line = line.replace('\t', "    ");
        let mut row = String::new();
        let mut used = 0;
        for glyph in Span::raw(&line).styled_graphemes(Style::new()) {
            let size = Span::raw(glyph.symbol).width();
            if used + size > capacity && !row.is_empty() {
                lines.push(std::mem::take(&mut row));
                used = 0;
            }
            row.push_str(glyph.symbol);
            used += size;
        }
        lines.push(row);
    }
    lines
        .into_iter()
        .map(|line| {
            Line::from(vec!["│ ".fg(MUTED), Span::raw(line)])
                .fg(INK)
                .bg(CONTROL)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{buffer::Buffer, style::Modifier, widgets::Widget};

    #[test]
    fn transcript_markdown_renders_structure_and_keeps_code_literal() {
        let source = "## Result\n\nKeep **durable writes** in `store.mjs`.\n- Restart the worker\n2. Reopen the page\n```js\n  const marker = '**literal**';\n```\n[reference](https://example.test)\n*quiet emphasis*";
        let rendered = lines(source, 60);
        assert_eq!(
            rendered.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "Result",
                "",
                "Keep durable writes in store.mjs.",
                "• Restart the worker",
                "2. Reopen the page",
                "js",
                "│   const marker = '**literal**';",
                "[reference](https://example.test)",
                "quiet emphasis",
            ]
        );
        for appearance in [
            crate::appearance::Appearance::Light,
            crate::appearance::Appearance::Dark,
        ] {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 60, 12));
            Paragraph::new(rendered.clone()).render(buffer.area, &mut buffer);
            let palette = theme::Palette::new(appearance, false);
            palette.paint(&mut buffer);
            assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
            assert!(buffer[(5, 2)].modifier.contains(Modifier::BOLD));
            assert_eq!(buffer[(23, 2)].bg, palette.foreground(CONTROL));
            assert_eq!(buffer[(4, 6)].bg, palette.foreground(CONTROL));
            assert_eq!(buffer[(4, 6)].symbol(), "c");
        }
    }

    #[test]
    fn output_preserves_spaces_unicode_and_markers_when_wrapping() {
        let rendered = code("  界界 = `**raw**`   end", 9);
        assert!(rendered.iter().all(|line| line.width() <= 9));
        assert_eq!(
            rendered
                .iter()
                .map(|line| line.to_string().strip_prefix("│ ").unwrap().to_owned())
                .collect::<String>(),
            "  界界 = `**raw**`   end"
        );
        let literal =
            "```js\nunfinished **bold\n\\`literal\\` and ``unsupported``\n***unsupported***";
        assert_eq!(
            lines(literal, 80)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
            literal
        );
    }

    #[test]
    fn emphasis_keeps_globs_and_math_literal() {
        let source = "Run *.js and *.mjs\n2 * 3 * 4\nKeep *quiet* and **durable** with ` x `.";
        let rendered = lines(source, 80);
        assert_eq!(
            rendered.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "Run *.js and *.mjs",
                "2 * 3 * 4",
                "Keep quiet and durable with  x .",
            ]
        );
        let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 3));
        Paragraph::new(rendered).render(buffer.area, &mut buffer);
        assert!(buffer[(5, 2)].modifier.contains(Modifier::ITALIC));
        assert!(buffer[(15, 2)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(28, 2)].bg, CONTROL);
    }
}
