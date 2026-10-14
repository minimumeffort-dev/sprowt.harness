use crate::appearance::Appearance;
use ratatui::{buffer::Buffer, style::Color};
use std::time::{Duration, Instant};

// Private render tokens, resolved once after drawing. Transcript strings never supply styles.
pub(super) const INK: Color = Color::Indexed(16);
pub(crate) const MUTED: Color = Color::Indexed(17);
pub(super) const BORDER: Color = Color::Indexed(18);
pub(super) const CONTROL: Color = Color::Indexed(19);
pub(super) const SELECTED: Color = Color::Indexed(20);
pub(super) const USER_BACKGROUND: Color = Color::Indexed(21);
pub(super) const ACCENT: Color = Color::Indexed(22);
pub(super) const BLUE: Color = Color::Indexed(24);
pub(super) const AMBER: Color = Color::Indexed(25);
pub(super) const RED: Color = Color::Indexed(26);
pub(super) const KEY_HINT: Color = MUTED;
pub(crate) const PET_BODY: Color = Color::Indexed(27);
pub(crate) const PET_INK: Color = Color::Indexed(28);
pub(crate) const PET_HIGHLIGHT: Color = Color::Indexed(29);
pub(crate) const PET_SHADE: Color = Color::Indexed(30);
pub(crate) const PET_LEAF: Color = Color::Indexed(31);

const PAPER: (u32, u32) = (0xfafbf8, 0x181c1a);
const COLORS: [(u32, u32); 16] = [
    (0x202923, 0xedf1eb), // ink
    (0x5b685e, 0xaab6ad), // muted
    (0x7d8b80, 0x728477), // control border
    (0xf0f3ee, 0x212723), // control background
    (0xdeeadf, 0x304338), // selected
    (0xeef3ed, 0x252f28), // user
    (0x296746, 0xa2d5b4), // accent / success
    (0xffffff, 0x19291f), // on accent
    (0x30668d, 0x9bc5e2), // working
    (0x825b12, 0xebc477), // attention
    (0xa13730, 0xefa39d), // failure
    (0x659973, 0xa2d5b4), // pet body
    (0x19291f, 0x19291f), // pet face
    (0x87b994, 0xc4e7cd), // pet highlight
    (0x467753, 0x79ab8b), // pet shade
    (0x296746, 0x86ba97), // pet leaf
];

pub(super) fn status_color(status: &str) -> Color {
    match status.to_ascii_lowercase().as_str() {
        "check failed" | "final checks failed" | "worker stopped" | "blocked" | "failed" => RED,
        "needs your answer" | "needs network access" => AMBER,
        "done" | "complete" => ACCENT,
        "working"
        | "checking"
        | "starting"
        | "resolving conflicts"
        | "connecting worker"
        | "starting repair"
        | "fixing failed check"
        | "restoring environment"
        | "stopping"
        | "routing"
        | "connecting"
        | "running" => BLUE,
        _ => MUTED,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Palette {
    dark: bool,
}

impl Palette {
    pub fn new(appearance: Appearance, system_dark: bool) -> Self {
        Self {
            dark: match appearance {
                Appearance::System => system_dark,
                Appearance::Light => false,
                Appearance::Dark => true,
            },
        }
    }

    fn rgb(self, pair: (u32, u32)) -> Color {
        let value = if self.dark { pair.1 } else { pair.0 };
        Color::Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
    }

    pub fn foreground(self, color: Color) -> Color {
        match color {
            Color::Reset => self.rgb(COLORS[0]),
            Color::Indexed(index @ 16..=31) => self.rgb(COLORS[(index - 16) as usize]),
            other => other,
        }
    }

    pub fn paint(self, buffer: &mut Buffer) {
        for cell in &mut buffer.content {
            cell.fg = self.foreground(cell.fg);
            cell.bg = if cell.bg == Color::Reset {
                self.rgb(PAPER)
            } else {
                self.foreground(cell.bg)
            };
        }
    }
}

pub(crate) struct SystemAppearance {
    pub dark: bool,
    checked: Option<Instant>,
}

impl Default for SystemAppearance {
    fn default() -> Self {
        Self {
            dark: true,
            checked: None,
        }
    }
}

impl SystemAppearance {
    pub fn refresh(&mut self) {
        if self
            .checked
            .is_some_and(|checked| checked.elapsed() < Duration::from_secs(2))
        {
            return;
        }
        self.checked = Some(Instant::now());
        self.dark = system_dark();
    }
}

#[cfg(target_os = "macos")]
fn system_dark() -> bool {
    // Poll only the presentation preference, at most once every two seconds.
    std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout.starts_with(b"Dark"))
}

#[cfg(not(target_os = "macos"))]
fn system_dark() -> bool {
    std::env::var("COLORFGBG")
        .ok()
        .and_then(|value| value.rsplit(';').next()?.parse::<u8>().ok())
        .is_none_or(|background| ![7, 15].contains(&background))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_fixture_records_the_resolved_terminal_palette_for_swift() {
        let mut palettes = serde_json::Map::new();
        for appearance in [Appearance::Light, Appearance::Dark] {
            let palette = Palette::new(appearance, false);
            let mut canvas = Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 1));
            palette.paint(&mut canvas);
            let mut colors = serde_json::Map::new();
            for (name, color) in [
                ("paper", canvas[(0, 0)].bg),
                ("ink", INK),
                ("muted", MUTED),
                ("control", BORDER),
                ("sidebar", CONTROL),
                ("selected", SELECTED),
                ("panel", USER_BACKGROUND),
                ("accent", ACCENT),
                ("blue", BLUE),
                ("workingStatus", status_color("Working")),
                ("attentionStatus", status_color("Needs your answer")),
                ("workerStatus", status_color("running")),
                ("amber", AMBER),
                ("red", RED),
            ] {
                let Color::Rgb(r, g, b) = palette.foreground(color) else {
                    panic!("Unresolved color");
                };
                colors.insert(name.into(), serde_json::json!([r, g, b]));
            }
            palettes.insert(appearance.label().to_ascii_lowercase(), colors.into());
        }
        let actual = serde_json::Value::Object(palettes);
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/appearance.json");
        if std::env::var_os("SPROWT_APPEARANCE_FIXTURE").is_some() {
            std::fs::write(
                &fixture,
                serde_json::to_string_pretty(&actual).unwrap() + "\n",
            )
            .unwrap();
        }
        let expected: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fixture).unwrap()).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            Palette::new(Appearance::System, false).foreground(INK),
            Color::Rgb(32, 41, 35)
        );
        assert_eq!(
            Palette::new(Appearance::System, true).foreground(INK),
            Color::Rgb(237, 241, 235)
        );
    }
}
