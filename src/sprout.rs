use std::time::Duration;

use ratatui::{Frame, layout::Rect, style::Color};

const BODY: Color = Color::Rgb(157, 190, 121);
const INK: Color = Color::Rgb(28, 51, 38);
const PIXELS: [&str; 8] = [
    "     T  ", "   HT   ", "  HBBB  ", " HBBBBB ", " HBBBBB ", " BBBBBB ", "  BSSB  ", "  S  S  ",
];
const HOP: [&str; 8] = [
    "   HT   ", "  HBBB  ", " HBBBBB ", " HBBBBB ", " BBBBBB ", "  BSSB  ", "  S  S  ", "        ",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mood {
    Idle,
    Planning,
    Working,
    Finished,
    Blocked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Face {
    Idle,
    HalfBlink,
    Blink,
    LookLeft,
    LookRight,
    Curious,
    Thoughtful,
    Focused,
    Happy,
    Wink,
    Concerned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pose {
    face: Face,
    leaf: i8,
    hop: bool,
}

impl Pose {
    pub const IDLE: Self = Self::new(Face::Idle, 0, false);

    const fn new(face: Face, leaf: i8, hop: bool) -> Self {
        Self { face, leaf, hop }
    }
}

pub fn animation(elapsed: Duration, mood: Mood, happy: Option<Duration>) -> (Pose, Duration) {
    if mood == Mood::Blocked {
        return (Pose::new(Face::Concerned, 0, false), Duration::from_secs(1));
    }
    let happy = if mood == Mood::Finished {
        Some(elapsed)
    } else if mood == Mood::Idle {
        happy
    } else {
        None
    };
    if let Some(happy) = happy
        && happy < Duration::from_millis(1_300)
    {
        let phase = happy.as_millis() as u64;
        let (face, hop, end) = match phase {
            0..150 => (Face::Happy, false, 150),
            150..300 => (Face::Happy, true, 300),
            300..650 => (Face::Happy, false, 650),
            650..900 => (Face::Wink, false, 900),
            _ => (Face::Happy, false, 1_300),
        };
        return (Pose::new(face, 0, hop), Duration::from_millis(end - phase));
    }

    let (face, leaf, phase, end) = match mood {
        Mood::Planning => {
            let phase = (elapsed.as_millis() % 4_200) as u64;
            let (face, leaf, end) = match phase {
                0..1_600 => (Face::Thoughtful, 0, 1_600),
                1_600..1_800 => (Face::Thoughtful, -1, 1_800),
                1_800..2_800 => (Face::Thoughtful, 0, 2_800),
                2_800..2_920 => (Face::Blink, 0, 2_920),
                _ => (Face::Thoughtful, 0, 4_200),
            };
            (face, leaf, phase, end)
        }
        Mood::Working => {
            let phase = (elapsed.as_millis() % 1_600) as u64;
            let (face, leaf, end) = match phase {
                0..400 => (Face::Focused, 0, 400),
                400..800 => (Face::Focused, -1, 800),
                800..1_200 => (Face::Focused, 0, 1_200),
                1_200..1_300 => (Face::Blink, 1, 1_300),
                _ => (Face::Focused, 1, 1_600),
            };
            (face, leaf, phase, end)
        }
        _ => {
            let phase = (elapsed.as_millis() % 14_000) as u64;
            let resting = if mood == Mood::Finished {
                Face::Happy
            } else {
                Face::Idle
            };
            let (face, leaf, end) = match phase {
                0..2_800 => (resting, 0, 2_800),
                2_800..2_880 => (Face::HalfBlink, 0, 2_880),
                2_880..3_000 => (Face::Blink, 0, 3_000),
                3_000..3_120 => (Face::HalfBlink, 0, 3_120),
                3_120..3_220 => (Face::Blink, 0, 3_220),
                3_220..3_300 => (Face::HalfBlink, 0, 3_300),
                3_300..5_600 => (resting, 0, 5_600),
                5_600..6_200 => (Face::LookLeft, 0, 6_200),
                6_200..6_900 => (Face::LookRight, 0, 6_900),
                6_900..7_500 => (Face::Curious, -1, 7_500),
                7_500..11_000 => (resting, 0, 11_000),
                11_000..11_200 => (resting, 1, 11_200),
                _ => (resting, 0, 14_000),
            };
            (face, leaf, phase, end)
        }
    };
    (
        Pose::new(face, leaf, false),
        Duration::from_millis(end - phase),
    )
}

pub fn draw(frame: &mut Frame, area: Rect, pose: Pose) {
    if area.width < 8 || area.height < 4 {
        return;
    }
    let pixels = if pose.hop { HOP } else { PIXELS };
    for y in 0..4 {
        for x in 0..8 {
            let top = color(pixel(&pixels, x, y * 2, pose.leaf));
            let bottom = color(pixel(&pixels, x, y * 2 + 1, pose.leaf));
            let cell = &mut frame.buffer_mut()[(area.x + x as u16, area.y + y as u16)];
            match (top, bottom) {
                (Some(top), Some(bottom)) => {
                    cell.set_symbol("▀").set_fg(top).set_bg(bottom);
                }
                (Some(top), None) => {
                    cell.set_symbol("▀").set_fg(top);
                }
                (None, Some(bottom)) => {
                    cell.set_symbol("▄").set_fg(bottom);
                }
                (None, None) => {}
            }
        }
    }

    let edges = if pose.hop {
        [(2, 0, "▗"), (5, 0, "▖"), (1, 1, "▐"), (6, 1, "▌")]
    } else {
        [(1, 1, "▗"), (6, 1, "▖"), (1, 2, "▐"), (6, 2, "▌")]
    };
    for (x, y, symbol) in edges {
        frame.buffer_mut()[(area.x + x, area.y + y)]
            .set_symbol(symbol)
            .set_fg(BODY)
            .set_bg(Color::Reset);
    }
    let (left_eye, mouth, right_eye) = match pose.face {
        Face::HalfBlink => ("•", "ᴗ", "•"),
        Face::Blink => ("─", "ᴗ", "─"),
        Face::LookLeft => ("◕", "ᴗ", "◕"),
        Face::LookRight => ("◔", "ᴗ", "◔"),
        Face::Curious => ("●", "o", "●"),
        Face::Thoughtful => ("◔", "·", "◔"),
        Face::Focused => ("•", "─", "•"),
        Face::Happy => ("^", "ᴗ", "^"),
        Face::Wink => ("●", "ᴗ", "─"),
        Face::Concerned => ("•", "⌒", "•"),
        _ => ("●", "ᴗ", "●"),
    };
    let (x, y) = (2, if pose.hop { 1 } else { 2 });
    for (x, symbol) in [(x, left_eye), (x + 1, mouth), (x + 2, right_eye)] {
        frame.buffer_mut()[(area.x + x, area.y + y)]
            .set_symbol(symbol)
            .set_fg(INK)
            .set_bg(BODY);
    }
}

fn pixel(pixels: &[&str; 8], x: usize, y: usize, leaf: i8) -> u8 {
    match (leaf, y, x) {
        (-1, 0, 5) | (1, 1, 4) => b' ',
        (-1, 0, 4) | (1, 1, 5) => b'T',
        _ => pixels[y].as_bytes()[x],
    }
}

fn color(pixel: u8) -> Option<Color> {
    match pixel {
        b'B' => Some(BODY),
        b'H' => Some(Color::Rgb(190, 215, 157)),
        b'S' => Some(Color::Rgb(106, 149, 88)),
        b'T' => Some(Color::Rgb(80, 158, 104)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn celebrations_never_mask_work_or_failure_and_do_not_repeat() {
        for mood in [Mood::Planning, Mood::Working, Mood::Blocked] {
            let elapsed = Duration::from_millis(200);
            assert_eq!(
                animation(elapsed, mood, Some(elapsed)),
                animation(elapsed, mood, None)
            );
        }
        assert!(
            animation(Duration::from_millis(200), Mood::Finished, None)
                .0
                .hop
        );
        for elapsed in [1_300, 14_200, 28_200] {
            assert!(
                !animation(Duration::from_millis(elapsed), Mood::Finished, None)
                    .0
                    .hop
            );
        }
    }

    #[test]
    fn every_animation_fits_the_same_terminal_footprint() {
        use ratatui::{Terminal, backend::TestBackend};

        for mood in [
            Mood::Idle,
            Mood::Planning,
            Mood::Working,
            Mood::Finished,
            Mood::Blocked,
        ] {
            let mut terminal = Terminal::new(TestBackend::new(10, 6)).unwrap();
            for millis in (0..14_000).step_by(80) {
                let (pose, delay) = animation(Duration::from_millis(millis), mood, None);
                assert!(delay > Duration::ZERO);
                terminal
                    .draw(|frame| draw(frame, Rect::new(1, 1, 8, 4), pose))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                for y in 0..6 {
                    for x in 0..10 {
                        if x == 0 || x == 9 || y == 0 || y == 5 {
                            assert_eq!(buffer[(x, y)].symbol(), " ");
                        }
                    }
                }
            }
        }
    }
}
