//! The tray glyph, drawn in code: "a." at rest and a wave of five bars while ahem works.
//! Shapes are in units of a 26 × 24 frame; edges are antialiased by their distance to the
//! pixel center.

/// The glyph's frame in units; a square icon takes its middle 24 × 24.
const FRAME_W: f32 = 26.0;
const FRAME_H: f32 = 24.0;

/// Wave bars: centers, width, and the height range while recording.
const BAR_X: [f32; 5] = [4.6, 8.8, 13.0, 17.2, 21.4];
const BAR_W: f32 = 2.2;
const BAR_MIN: f32 = 4.0;
const BAR_MAX: f32 = 18.0;
const DOT_R: f32 = 1.5;

/// How high a bar reaches, 0–1, for a microphone level (RMS, 0–1): the decibel scale of the
/// indicator's capsule (`meter` in `present.ts`). Room noise (−50 dBFS) is empty, loud speech
/// (−12 dBFS) full.
fn meter(level: f32) -> f32 {
    if level <= 0.0 {
        return 0.0;
    }
    ((20.0 * level.log10() + 50.0) / 38.0).clamp(0.0, 1.0)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Glyph {
    /// "a." — waiting for a hotkey.
    Idle,
    /// Bars follow the input level, 0–1, the newest on the right.
    Recording([f32; 5]),
    /// The wave settles into dots that light up one by one; `lit` is 0–4.
    Busy { lit: usize },
    /// Dimmed dots with "!" in the middle.
    Problem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// `ink` draws the letter and the dots, `live` the recording wave and the "!".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub ink: Rgb,
    pub live: Rgb,
}

impl Palette {
    /// macOS paints a template image in the menu bar's own colors: only its alpha counts.
    pub const TEMPLATE: Self = Self { ink: Rgb(0, 0, 0), live: Rgb(0, 0, 0) };

    /// The text color of the surface and the one accent of the interface, `#FF5A00`.
    pub fn colored(light_surface: bool) -> Self {
        let ink = if light_surface { Rgb(0x0a, 0x0a, 0x0a) } else { Rgb(0xf5, 0xf5, 0xf5) };
        Self { ink, live: Rgb(0xff, 0x5a, 0x00) }
    }
}

#[derive(Debug, Clone, Copy)]
enum Shape {
    Disc {
        cx: f32,
        cy: f32,
        r: f32,
    },
    /// A circle stroke: `r` to the middle of the stroke, `half` its half width.
    Ring {
        cx: f32,
        cy: f32,
        r: f32,
        half: f32,
    },
    /// A rectangle with rounded corners of `radius`.
    Rect {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        radius: f32,
    },
}

impl Shape {
    /// Signed distance in units: negative inside.
    fn distance(self, x: f32, y: f32) -> f32 {
        match self {
            Self::Disc { cx, cy, r } => (x - cx).hypot(y - cy) - r,
            Self::Ring { cx, cy, r, half } => ((x - cx).hypot(y - cy) - r).abs() - half,
            Self::Rect { x0, y0, x1, y1, radius } => {
                let qx = (x - (x0 + x1) / 2.0).abs() - ((x1 - x0) / 2.0 - radius);
                let qy = (y - (y0 + y1) / 2.0).abs() - ((y1 - y0) / 2.0 - radius);
                qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
            }
        }
    }
}

struct Mark {
    shape: Shape,
    color: Rgb,
    alpha: f32,
}

/// A bar of the wave around the middle line; a bar as high as it is wide is a dot.
fn bar(index: usize, height: f32) -> Shape {
    let half = BAR_W / 2.0;
    let x = BAR_X[index];
    let y = FRAME_H / 2.0;
    Shape::Rect {
        x0: x - half,
        y0: y - height / 2.0,
        x1: x + half,
        y1: y + height / 2.0,
        radius: half,
    }
}

fn dot(index: usize) -> Shape {
    Shape::Disc { cx: BAR_X[index], cy: FRAME_H / 2.0, r: DOT_R }
}

fn marks(glyph: Glyph, palette: Palette) -> Vec<Mark> {
    let mark = |shape, color, alpha| Mark { shape, color, alpha };
    match glyph {
        Glyph::Idle => vec![
            mark(Shape::Ring { cx: 8.0, cy: 12.0, r: 4.9, half: 1.3 }, palette.ink, 1.0),
            mark(
                Shape::Rect { x0: 11.6, y0: 6.1, x1: 14.2, y1: 17.9, radius: 0.0 },
                palette.ink,
                1.0,
            ),
            mark(Shape::Disc { cx: 17.5, cy: 16.5, r: 1.7 }, palette.ink, 1.0),
        ],
        Glyph::Recording(levels) => levels
            .iter()
            .enumerate()
            .map(|(index, level)| {
                mark(bar(index, BAR_MIN + (BAR_MAX - BAR_MIN) * meter(*level)), palette.live, 1.0)
            })
            .collect(),
        Glyph::Busy { lit } => (0..BAR_X.len())
            .map(|index| mark(dot(index), palette.ink, if index == lit % 5 { 1.0 } else { 0.35 }))
            .collect(),
        Glyph::Problem => {
            let mut marks: Vec<Mark> =
                [0, 1, 3, 4].iter().map(|&index| mark(dot(index), palette.ink, 0.4)).collect();
            let x = BAR_X[2];
            let half = BAR_W / 2.0;
            marks.push(mark(
                Shape::Rect { x0: x - half, y0: 4.4, x1: x + half, y1: 14.0, radius: half },
                palette.live,
                1.0,
            ));
            marks.push(mark(Shape::Disc { cx: x, cy: 17.8, r: 1.4 }, palette.live, 1.0));
            marks
        }
    }
}

/// Draws the glyph into a `width` × `height` RGBA image (not premultiplied). A square image
/// takes the middle 24 × 24 of the frame, so the glyph fills more of it.
pub fn render(glyph: Glyph, palette: Palette, width: u32, height: u32) -> Vec<u8> {
    let (view_x, view_w) = if width == height { (1.0, FRAME_H) } else { (0.0, FRAME_W) };
    let scale = (width as f32 / view_w).min(height as f32 / FRAME_H);
    let left = (width as f32 - view_w * scale) / 2.0;
    let top = (height as f32 - FRAME_H * scale) / 2.0;
    let marks = marks(glyph, palette);
    let mut image = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        for column in 0..width {
            let x = (column as f32 + 0.5 - left) / scale + view_x;
            let y = (row as f32 + 0.5 - top) / scale;
            // Premultiplied "over" of every mark in order.
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for mark in &marks {
                let coverage = (0.5 - mark.shape.distance(x, y) * scale).clamp(0.0, 1.0);
                let alpha = coverage * mark.alpha;
                if alpha > 0.0 {
                    let Rgb(mr, mg, mb) = mark.color;
                    r = f32::from(mr) * alpha + r * (1.0 - alpha);
                    g = f32::from(mg) * alpha + g * (1.0 - alpha);
                    b = f32::from(mb) * alpha + b * (1.0 - alpha);
                    a = alpha + a * (1.0 - alpha);
                }
            }
            if a > 0.0 {
                image.extend([r / a, g / a, b / a, a * 255.0].map(|channel| channel.round() as u8));
            } else {
                image.extend([0, 0, 0, 0]);
            }
        }
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (u32, u32) = (39, 36);

    fn pixel(image: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * width + x) * 4) as usize;
        [image[at], image[at + 1], image[at + 2], image[at + 3]]
    }

    fn opaque_pixels(image: &[u8]) -> usize {
        image.chunks(4).filter(|pixel| pixel[3] > 128).count()
    }

    /// The pixel at a point of the 26 × 24 frame in a 39 × 36 image (1.5 pixels per unit).
    fn at(image: &[u8], x: f32, y: f32) -> [u8; 4] {
        pixel(image, SIZE.0, (x * 1.5) as u32, (y * 1.5) as u32)
    }

    #[test]
    fn idle_draws_the_letter_with_its_period() {
        let palette = Palette::colored(true);
        let image = render(Glyph::Idle, palette, SIZE.0, SIZE.1);
        assert_eq!(image.len(), (SIZE.0 * SIZE.1 * 4) as usize);
        // The stem, the period, and the hole of the letter.
        assert_eq!(at(&image, 12.9, 12.0), [0x0a, 0x0a, 0x0a, 255]);
        assert_eq!(at(&image, 17.5, 16.5)[3], 255);
        assert_eq!(at(&image, 8.0, 12.0)[3], 0);
    }

    /// The same points as the capsule's `meter` test in `present.test.ts`.
    #[test]
    fn the_wave_uses_the_decibel_scale_of_the_capsule() {
        assert_eq!(meter(0.0), 0.0);
        assert_eq!(meter(0.002), 0.0);
        assert!(meter(0.03) > 0.45 && meter(0.03) < 0.6);
        assert!(meter(0.1) > 0.75);
        assert_eq!(meter(0.5), 1.0);
    }

    #[test]
    fn the_wave_grows_with_the_level_and_stays_visible_in_silence() {
        let palette = Palette::colored(false);
        let silent = render(Glyph::Recording([0.0; 5]), palette, SIZE.0, SIZE.1);
        let loud = render(Glyph::Recording([1.0; 5]), palette, SIZE.0, SIZE.1);
        assert!(opaque_pixels(&silent) > 0);
        assert!(opaque_pixels(&loud) > 2 * opaque_pixels(&silent));
        // Recording is drawn in the live color.
        assert_eq!(at(&loud, 13.0, 12.0), [0xff, 0x5a, 0x00, 255]);
    }

    #[test]
    fn busy_dots_light_up_in_turn() {
        let palette = Palette::colored(true);
        let first = render(Glyph::Busy { lit: 0 }, palette, SIZE.0, SIZE.1);
        let second = render(Glyph::Busy { lit: 1 }, palette, SIZE.0, SIZE.1);
        assert_eq!(at(&first, 4.6, 12.0)[3], 255);
        assert!(at(&second, 4.6, 12.0)[3] < 128);
        assert_eq!(at(&second, 8.8, 12.0)[3], 255);
    }

    #[test]
    fn a_problem_puts_an_exclamation_mark_into_the_wave() {
        let palette = Palette::colored(true);
        let image = render(Glyph::Problem, palette, SIZE.0, SIZE.1);
        assert_eq!(at(&image, 13.0, 8.0), [0xff, 0x5a, 0x00, 255]);
        assert_eq!(at(&image, 13.0, 17.8), [0xff, 0x5a, 0x00, 255]);
        let dimmed = at(&image, 4.6, 12.0)[3];
        assert!(dimmed > 0 && dimmed < 128);
    }

    #[test]
    fn a_template_is_black_with_alpha() {
        for glyph in
            [Glyph::Idle, Glyph::Recording([0.5; 5]), Glyph::Busy { lit: 2 }, Glyph::Problem]
        {
            let image = render(glyph, Palette::TEMPLATE, SIZE.0, SIZE.1);
            assert!(opaque_pixels(&image) > 0);
            assert!(image.chunks(4).all(|pixel| pixel[..3] == [0, 0, 0]));
        }
    }

    #[test]
    fn a_square_icon_keeps_the_glyph_inside() {
        let image = render(Glyph::Recording([1.0; 5]), Palette::colored(false), 16, 16);
        assert_eq!(image.len(), 16 * 16 * 4);
        assert!(opaque_pixels(&image) > 0);
        // The outer bars stay off the left and right edges.
        for row in 0..16 {
            assert_eq!(pixel(&image, 16, 0, row)[3], 0);
            assert_eq!(pixel(&image, 16, 15, row)[3], 0);
        }
    }
}
