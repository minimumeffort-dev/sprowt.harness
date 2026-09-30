use std::time::Duration;

use ratatui::{Frame, layout::Rect, style::Color};

const BODY: Color = Color::Rgb(157, 190, 121);
const INK: Color = Color::Rgb(28, 51, 38);
const PIXELS: [&str; 8] = [
    "     T  ", "   HT   ", "  HBBB  ", " HBBBBB ", " HBBBBB ", " BBBBBB ", "  BSSB  ", "  S  S  ",
];
const NOD: [&str; 8] = [
    "        ", "     T  ", "   HT   ", " HBBBBB ", " HBBBBB ", " BBBBBB ", "  BSSB  ", "  S  S  ",
];
const HOP: [&str; 8] = [
    "   HT   ", "  HBBB  ", " HBBBBB ", " BBBBBB ", "  BSSB  ", "  S  S  ", "        ", "        ",
];

#[derive(Clone, Copy)]
pub enum Pose {
    Idle,
    HalfBlink,
    Blink,
    Curious,
    Cheerful,
    Sleepy,
    Nod,
    Hop,
    Wink,
}

pub fn animation(elapsed: Duration, happy: Option<Duration>) -> (Pose, Duration) {
    if let Some(happy) = happy
        && happy < Duration::from_millis(1_300)
    {
        let phase = happy.as_millis() as u64;
        let (pose, end) = match phase {
            0..120 => (Pose::Nod, 120),
            120..320 => (Pose::Hop, 320),
            320..440 => (Pose::Nod, 440),
            440..700 => (Pose::Cheerful, 700),
            700..1_120 => (Pose::Wink, 1_120),
            _ => (Pose::Cheerful, 1_300),
        };
        return (pose, Duration::from_millis(end - phase));
    }

    let phase = (elapsed.as_millis() % 18_000) as u64;
    let (pose, end) = match phase {
        0..2_800 => (Pose::Idle, 2_800),
        2_800..2_880 => (Pose::HalfBlink, 2_880),
        2_880..3_000 => (Pose::Blink, 3_000),
        3_000..3_080 => (Pose::HalfBlink, 3_080),
        3_080..5_600 => (Pose::Idle, 5_600),
        5_600..5_760 => (Pose::Nod, 5_760),
        5_760..5_960 => (Pose::Idle, 5_960),
        5_960..7_060 => (Pose::Curious, 7_060),
        7_060..9_000 => (Pose::Idle, 9_000),
        9_000..9_120 => (Pose::Nod, 9_120),
        9_120..9_320 => (Pose::Hop, 9_320),
        9_320..9_440 => (Pose::Nod, 9_440),
        9_440..10_200 => (Pose::Cheerful, 10_200),
        10_200..14_300 => (Pose::Idle, 14_300),
        14_300..15_200 => (Pose::Sleepy, 15_200),
        15_200..15_300 => (Pose::HalfBlink, 15_300),
        _ => (Pose::Idle, 18_000),
    };
    (pose, Duration::from_millis(end - phase))
}

pub fn draw(frame: &mut Frame, area: Rect, pose: Pose) {
    if area.width < 8 || area.height < 4 {
        return;
    }
    let pixels = match pose {
        Pose::Nod => NOD,
        Pose::Hop => HOP,
        _ => PIXELS,
    };
    for y in 0..4 {
        for x in 0..8 {
            let top = color(pixels[y * 2].as_bytes()[x]);
            let bottom = color(pixels[y * 2 + 1].as_bytes()[x]);
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

    let edges = match pose {
        Pose::Hop => [(2, 0, "▗"), (5, 0, "▖"), (1, 1, "▐"), (6, 1, "▌")],
        _ => [(1, 1, "▗"), (6, 1, "▖"), (1, 2, "▐"), (6, 2, "▌")],
    };
    for (x, y, symbol) in edges {
        frame.buffer_mut()[(area.x + x, area.y + y)]
            .set_symbol(symbol)
            .set_fg(BODY)
            .set_bg(Color::Reset);
    }
    let (left_eye, mouth, right_eye) = match pose {
        Pose::HalfBlink => ("•", "ᴗ", "•"),
        Pose::Blink => ("─", "ᴗ", "─"),
        Pose::Curious => ("●", "o", "●"),
        Pose::Cheerful => ("^", "ᴗ", "^"),
        Pose::Sleepy => ("─", "·", "─"),
        Pose::Wink => ("●", "ᴗ", "─"),
        _ => ("●", "ᴗ", "●"),
    };
    let (x, y) = match pose {
        Pose::Hop => (2, 1),
        _ => (2, 2),
    };
    for (x, symbol) in [(x, left_eye), (x + 1, mouth), (x + 2, right_eye)] {
        frame.buffer_mut()[(area.x + x, area.y + y)]
            .set_symbol(symbol)
            .set_fg(INK)
            .set_bg(BODY);
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
