use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Clone, Serialize)]
pub struct MessageContent {
    pub id: String,
    pub segments: Vec<Segment>,
    #[serde(skip)]
    pub key: u128,
}

#[derive(Clone, Serialize)]
pub struct Segment {
    pub id: usize,
    #[serde(flatten)]
    pub content: Content,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Content {
    Prose {
        text: String,
        preview: Option<String>,
    },
    Code {
        source: String,
        language: String,
        summary: String,
        collapsed: bool,
        tokens: Vec<Token>,
    },
}

#[derive(Clone, Serialize)]
pub struct Token {
    pub text: String,
    pub kind: TokenKind,
}

#[derive(Clone, Copy, Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TokenKind {
    Plain,
    Key,
    String,
    Number,
    Literal,
    Punctuation,
}

pub fn messages(messages: &[crate::store::Message]) -> Vec<MessageContent> {
    let mut occurrences = HashMap::new();
    messages
        .iter()
        .map(|message| {
            let identity = serde_json::to_vec(&(
                &message.source,
                message.task,
                &message.item_id,
                &message.role,
                if message.item_id.is_some() {
                    ""
                } else {
                    &message.body
                },
            ))
            .unwrap();
            let occurrence = occurrences.entry(identity.clone()).or_insert(0usize);
            let content = MessageContent::new(&message.body, &identity, *occurrence);
            *occurrence += 1;
            content
        })
        .collect()
}

impl MessageContent {
    pub fn new(source: &str, identity: &[u8], occurrence: usize) -> Self {
        let mut digest = Sha256::new();
        digest.update(identity);
        digest.update(occurrence.to_le_bytes());
        let key = u128::from_le_bytes(digest.finalize()[..16].try_into().unwrap());
        Self {
            id: format!("{key:032x}"),
            key,
            segments: parse(source),
        }
    }

    pub fn task_report(mut self, message: &crate::store::Message) -> Self {
        if message
            .item_id
            .as_deref()
            .is_some_and(|id| id.starts_with("result:"))
            && let Some((header, body)) = message.body.split_once('\n')
        {
            self.segments = parse(body);
            for segment in &mut self.segments {
                segment.id += header.len() + 1;
            }
        }
        self
    }
}

impl Segment {
    pub fn initially_collapsed(&self) -> bool {
        match &self.content {
            Content::Prose { preview, .. } => preview.is_some(),
            Content::Code { collapsed, .. } => *collapsed,
        }
    }
}

fn prose(segments: &mut Vec<Segment>, start: usize, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    let text = text.trim().to_owned();
    let long = text.chars().count() > 480 || text.lines().count() > 6;
    let preview = long.then(|| {
        let mut end = text.len();
        let mut lines = 0;
        for (count, (index, c)) in text.char_indices().enumerate() {
            if c == '\n' {
                lines += 1;
            }
            if count == 360 || lines == 4 {
                end = index;
                break;
            }
        }
        let prefix = &text[..end];
        let end = prefix
            .rfind(char::is_whitespace)
            .filter(|i| *i > prefix.len() / 2)
            .unwrap_or(prefix.len());
        prefix[..end].trim_end().to_owned()
    });
    segments.push(Segment {
        id: start,
        content: Content::Prose { text, preview },
    });
}

fn code(segments: &mut Vec<Segment>, start: usize, source: &str, language: &str) {
    let json = serde_json::from_str::<serde_json::Value>(source).is_ok();
    let language = if json && (language.is_empty() || language.eq_ignore_ascii_case("json")) {
        "JSON"
    } else {
        language
    };
    let lines = source.lines().count().max(1);
    let size = if source.len() >= 1000 {
        format!("{:.1} KB", source.len() as f64 / 1000.0)
    } else {
        format!("{} B", source.len())
    };
    let label = if language.is_empty() {
        "Code"
    } else {
        language
    };
    segments.push(Segment {
        id: start,
        content: Content::Code {
            source: source.to_owned(),
            language: language.to_owned(),
            summary: format!(
                "{label} · {lines} {} · {size}",
                if lines == 1 { "line" } else { "lines" }
            ),
            collapsed: language == "JSON" || lines > 12 || source.len() > 1200,
            tokens: if json && language == "JSON" {
                json_tokens(source)
            } else {
                vec![]
            },
        },
    });
}

fn parse(source: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut cursor = 0;
    let mut prose_start = 0;
    while cursor < source.len() {
        let line_end = source[cursor..]
            .find('\n')
            .map_or(source.len(), |n| cursor + n + 1);
        let line = source[cursor..line_end].trim_end_matches(['\r', '\n']);
        let fence = line.bytes().take_while(|c| *c == b'`').count();
        if fence >= 3 && !line[fence..].contains('`') {
            let mut close = line_end;
            let mut matched = false;
            while close < source.len() {
                let end = source[close..]
                    .find('\n')
                    .map_or(source.len(), |n| close + n + 1);
                let closing = source[close..end].trim();
                if closing.len() >= fence && closing.bytes().all(|c| c == b'`') {
                    prose(&mut segments, prose_start, &source[prose_start..cursor]);
                    code(
                        &mut segments,
                        cursor,
                        &source[line_end..close],
                        line[fence..].trim(),
                    );
                    cursor = end;
                    prose_start = end;
                    matched = true;
                    break;
                }
                close = end;
            }
            if matched {
                continue;
            }
            // An unfinished fence is literal prose; never discover JSON inside it.
            break;
        }
        let candidate = line.trim_start();
        if candidate.starts_with(['{', '[']) {
            let value_start = cursor + line.len() - candidate.len();
            let mut stream = serde_json::Deserializer::from_str(&source[value_start..])
                .into_iter::<serde_json::Value>();
            if stream.next().is_some_and(|value| value.is_ok()) {
                let value_end = value_start + stream.byte_offset();
                let end = source[value_end..]
                    .find('\n')
                    .map_or(source.len(), |n| value_end + n + 1);
                if source[value_end..end].trim().is_empty() {
                    prose(&mut segments, prose_start, &source[prose_start..cursor]);
                    code(&mut segments, cursor, &source[cursor..end], "JSON");
                    cursor = end;
                    prose_start = end;
                    continue;
                }
            }
        }
        cursor = line_end;
    }
    prose(&mut segments, prose_start, &source[prose_start..]);
    segments
}

fn json_tokens(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let mut end = start + 1;
        let kind = match bytes[start] {
            b'"' => {
                while end < bytes.len() {
                    match bytes[end] {
                        b'\\' => end += 2,
                        b'"' => {
                            end += 1;
                            break;
                        }
                        _ => end += 1,
                    }
                }
                if source[end..].trim_start().starts_with(':') {
                    TokenKind::Key
                } else {
                    TokenKind::String
                }
            }
            b'-' | b'0'..=b'9' => {
                while end < bytes.len()
                    && matches!(bytes[end], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    end += 1;
                }
                TokenKind::Number
            }
            b't' | b'f' | b'n' => {
                while end < bytes.len() && bytes[end].is_ascii_alphabetic() {
                    end += 1;
                }
                TokenKind::Literal
            }
            b'{' | b'}' | b'[' | b']' | b':' | b',' => TokenKind::Punctuation,
            _ => {
                while end < bytes.len() && bytes[end].is_ascii_whitespace() {
                    end += 1;
                }
                TokenKind::Plain
            }
        };
        tokens.push(Token {
            text: source[start..end].to_owned(),
            kind,
        });
        start = end;
    }
    tokens
}

#[cfg(test)]
mod tests;
