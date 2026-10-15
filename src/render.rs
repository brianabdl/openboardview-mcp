//! Renders a board view to PNG.
//!
//! The view follows OpenBoardView's conventions: Y grows upward, and the
//! bottom side is mirrored horizontally, as if the board had been flipped
//! over. Labels use an embedded 8x8 bitmap font, so no system fonts are needed.

use anyhow::{Result, anyhow};
use font8x8::UnicodeFonts;
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::board::{Board, NetKind, Point, Rect, Side};

pub const MAX_DIMENSION: u32 = 2048;
pub const MIN_DIMENSION: u32 = 256;

/// Colors assigned to highlighted nets, in order.
const NET_PALETTE: &[(u8, u8, u8)] = &[
    (255, 214, 0),   // yellow
    (0, 229, 255),   // cyan
    (255, 64, 129),  // pink
    (118, 255, 3),   // green
    (255, 145, 0),   // orange
    (179, 136, 255), // violet
];
const PART_HIGHLIGHT: (u8, u8, u8) = (255, 82, 82);

#[derive(Debug, Clone)]
pub struct RenderRequest {
    pub side: Side,
    pub width: u32,
    pub highlight_parts: Vec<usize>,
    pub highlight_nets: Vec<usize>,
    /// Region to show; `None` shows the whole board.
    pub viewport: Option<Rect>,
    /// Label every visible part (otherwise only highlighted parts, or all
    /// when the view is zoomed in far enough to fit them).
    pub label_all: bool,
}

#[derive(Debug, Clone)]
pub struct RenderOutput {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub viewport: Rect,
    pub pixels_per_mil: f64,
    pub labeled_parts: usize,
    /// (net name, "#rrggbb") for each highlighted net.
    pub legend: Vec<(String, String)>,
}

struct View {
    viewport: Rect,
    scale: f64,
    mirror: bool,
}

impl View {
    fn to_screen(&self, p: Point) -> (f32, f32) {
        let x = if self.mirror { self.viewport.max.x - p.x } else { p.x - self.viewport.min.x };
        let y = self.viewport.max.y - p.y;
        ((x * self.scale) as f32, (y * self.scale) as f32)
    }

    fn screen_rect(&self, r: &Rect) -> Option<tiny_skia::Rect> {
        let (x1, y1) = self.to_screen(r.min);
        let (x2, y2) = self.to_screen(r.max);
        tiny_skia::Rect::from_ltrb(
            x1.min(x2),
            y1.min(y2),
            x1.max(x2).max(x1.min(x2) + 1.0),
            y1.max(y2).max(y1.min(y2) + 1.0),
        )
    }
}

fn paint(rgb: (u8, u8, u8), alpha: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(rgb.0, rgb.1, rgb.2, alpha);
    p.anti_alias = true;
    p
}

fn hex(rgb: (u8, u8, u8)) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2)
}

pub fn render(board: &Board, req: &RenderRequest) -> Result<RenderOutput> {
    let mut viewport = req.viewport.unwrap_or(board.bbox);
    if viewport.is_empty() {
        return Err(anyhow!("board has no geometry to render"));
    }
    // Avoid degenerate (zero-area) views, e.g. focusing on a single pin.
    let min_span = 100.0;
    if viewport.width() < min_span || viewport.height() < min_span {
        let c = viewport.center();
        let half = min_span.max(viewport.width()).max(viewport.height()) / 2.0;
        viewport = Rect { min: Point::new(c.x - half, c.y - half), max: Point::new(c.x + half, c.y + half) };
    }

    let width = req.width.clamp(MIN_DIMENSION, MAX_DIMENSION);
    let mut scale = width as f64 / viewport.width();
    let mut height = (viewport.height() * scale).ceil() as u32;
    if height > MAX_DIMENSION {
        scale = MAX_DIMENSION as f64 / viewport.height();
        height = MAX_DIMENSION;
    }
    let height = height.max(64);
    let width = ((viewport.width() * scale).ceil() as u32).clamp(64, MAX_DIMENSION);

    let view = View { viewport, scale, mirror: req.side == Side::Bottom };
    let mut pixmap = Pixmap::new(width, height).ok_or_else(|| anyhow!("cannot allocate {width}x{height} image"))?;
    pixmap.fill(Color::from_rgba8(16, 20, 24, 255));

    let identity = Transform::identity();
    let thin = Stroke { width: 1.0, ..Stroke::default() };

    // Outline.
    {
        let mut pb = PathBuilder::new();
        let mut pts = board.outline_points.iter();
        if let Some(&first) = pts.next() {
            let (x, y) = view.to_screen(first);
            pb.move_to(x, y);
            for &p in pts {
                let (x, y) = view.to_screen(p);
                pb.line_to(x, y);
            }
        }
        for &(a, b) in &board.outline_segments {
            let (x1, y1) = view.to_screen(a);
            let (x2, y2) = view.to_screen(b);
            pb.move_to(x1, y1);
            pb.line_to(x2, y2);
        }
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(
                &path,
                &paint((122, 138, 153), 255),
                &Stroke { width: 1.5, ..Stroke::default() },
                identity,
                None,
            );
        }
    }

    let visible_view = viewport.expanded(50.0);
    let in_view = |p: Point| {
        p.x >= visible_view.min.x && p.x <= visible_view.max.x && p.y >= visible_view.min.y && p.y <= visible_view.max.y
    };
    let rect_in_view = |r: &Rect| {
        !r.is_empty()
            && r.max.x >= visible_view.min.x
            && r.min.x <= visible_view.max.x
            && r.max.y >= visible_view.min.y
            && r.min.y <= visible_view.max.y
    };

    // Part bodies.
    let mut body = PathBuilder::new();
    let mut visible_parts = Vec::new();
    for (idx, part) in board.parts.iter().enumerate() {
        if !part.side.visible_from(req.side) || !rect_in_view(&part.bbox) {
            continue;
        }
        visible_parts.push(idx);
        if let Some(r) = view.screen_rect(&part.bbox) {
            body.push_rect(r);
        }
    }
    if let Some(path) = body.finish() {
        pixmap.stroke_path(&path, &paint((70, 86, 102), 255), &thin, identity, None);
    }

    // Pins, batched per color.
    let net_color: Vec<Option<(u8, u8, u8)>> = {
        let mut colors = vec![None; board.nets.len()];
        for (i, &net) in req.highlight_nets.iter().enumerate() {
            colors[net] = Some(NET_PALETTE[i % NET_PALETTE.len()]);
        }
        colors
    };
    let min_pin_px = if scale > 0.5 { 2.0 } else { 1.0 };
    let mut ground = PathBuilder::new();
    let mut power = PathBuilder::new();
    let mut other = PathBuilder::new();
    let mut highlighted: Vec<(PathBuilder, (u8, u8, u8))> = Vec::new();
    for pin in &board.pins {
        if !pin.side.visible_from(req.side) || !in_view(pin.pos) {
            continue;
        }
        let (x, y) = view.to_screen(pin.pos);
        let r = ((pin.radius.max(3.0) * scale) as f32).max(min_pin_px);
        if let Some(color) = net_color[pin.net] {
            let entry = match highlighted.iter_mut().position(|(_, c)| *c == color) {
                Some(i) => &mut highlighted[i].0,
                None => {
                    highlighted.push((PathBuilder::new(), color));
                    &mut highlighted.last_mut().unwrap().0
                }
            };
            entry.push_circle(x, y, r.max(3.0) * 1.4);
            continue;
        }
        let target = match board.nets[pin.net].kind {
            NetKind::Ground => &mut ground,
            NetKind::Power => &mut power,
            _ => &mut other,
        };
        target.push_circle(x, y, r);
    }
    let dim = !req.highlight_nets.is_empty() || !req.highlight_parts.is_empty();
    let alpha = if dim { 110 } else { 255 };
    for (pb, rgb) in [(ground, (46, 125, 82)), (power, (198, 80, 80)), (other, (150, 160, 175))] {
        if let Some(path) = pb.finish() {
            pixmap.fill_path(&path, &paint(rgb, alpha), FillRule::Winding, identity, None);
        }
    }

    // Highlighted parts.
    for &idx in &req.highlight_parts {
        let part = &board.parts[idx];
        if let Some(r) = view.screen_rect(&part.bbox.expanded(4.0)) {
            let path = PathBuilder::from_rect(r);
            pixmap.fill_path(&path, &paint(PART_HIGHLIGHT, 70), FillRule::Winding, identity, None);
            pixmap.stroke_path(
                &path,
                &paint(PART_HIGHLIGHT, 255),
                &Stroke { width: 2.0, ..Stroke::default() },
                identity,
                None,
            );
        }
    }

    // Highlighted nets on top.
    for (pb, rgb) in highlighted {
        if let Some(path) = pb.finish() {
            pixmap.fill_path(&path, &paint(rgb, 255), FillRule::Winding, identity, None);
            pixmap.stroke_path(&path, &paint((0, 0, 0), 255), &thin, identity, None);
        }
    }

    // Labels.
    let text_scale = if width >= 1600 { 2 } else { 1 };
    let max_auto_labels = (width as usize * height as usize) / 9000;
    let label_everything = req.label_all || visible_parts.len() <= max_auto_labels.min(250);
    let mut labeled = 0;
    let mut label = |pixmap: &mut Pixmap, idx: usize, rgb: (u8, u8, u8)| {
        let part = &board.parts[idx];
        let (x, y) = view.to_screen(part.center());
        let w = (part.name.chars().count() * 8 * text_scale) as f32;
        draw_text(pixmap, &part.name, x - w / 2.0, y - (4 * text_scale) as f32, text_scale, rgb);
        labeled += 1;
    };
    if label_everything {
        for &idx in &visible_parts {
            if !req.highlight_parts.contains(&idx) {
                label(&mut pixmap, idx, (205, 214, 224));
            }
        }
    }
    for &idx in &req.highlight_parts {
        label(&mut pixmap, idx, (255, 255, 255));
    }

    // Header: side and scale bar.
    let side_text = match req.side {
        Side::Top => "TOP",
        Side::Bottom => "BOTTOM (mirrored)",
        Side::Both => "BOTH SIDES",
    };
    draw_text(&mut pixmap, side_text, 8.0, 8.0, text_scale, (230, 230, 230));
    draw_scale_bar(&mut pixmap, scale, text_scale);

    let legend = req
        .highlight_nets
        .iter()
        .enumerate()
        .map(|(i, &net)| (board.nets[net].name.clone(), hex(NET_PALETTE[i % NET_PALETTE.len()])))
        .collect();

    let png = pixmap.encode_png().map_err(|e| anyhow!("PNG encoding failed: {e}"))?;
    Ok(RenderOutput { png, width, height, viewport, pixels_per_mil: scale, labeled_parts: labeled, legend })
}

/// Draws text with a dark backdrop so it stays legible over pins.
fn draw_text(pixmap: &mut Pixmap, text: &str, x: f32, y: f32, scale: usize, rgb: (u8, u8, u8)) {
    let glyph_px = 8 * scale;
    let width = text.chars().count() * glyph_px;
    if let Some(r) = tiny_skia::Rect::from_xywh(x - 1.0, y - 1.0, width as f32 + 2.0, glyph_px as f32 + 2.0) {
        pixmap.fill_rect(r, &paint((0, 0, 0), 150), Transform::identity(), None);
    }
    let fg = paint(rgb, 255);
    let mut pb = PathBuilder::new();
    for (i, ch) in text.chars().enumerate() {
        let glyph = font8x8::BASIC_FONTS.get(ch).or_else(|| font8x8::BASIC_FONTS.get('?')).unwrap_or([0; 8]);
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..8 {
                if bits & (1 << col) != 0 {
                    let px = x + ((i * 8 + col) * scale) as f32;
                    let py = y + (row * scale) as f32;
                    if let Some(r) = tiny_skia::Rect::from_xywh(px, py, scale as f32, scale as f32) {
                        pb.push_rect(r);
                    }
                }
            }
        }
    }
    if let Some(path) = pb.finish() {
        let mut fg = fg;
        fg.anti_alias = false;
        pixmap.fill_path(&path, &fg, FillRule::Winding, Transform::identity(), None);
    }
}

fn draw_scale_bar(pixmap: &mut Pixmap, pixels_per_mil: f64, text_scale: usize) {
    let px_per_mm = pixels_per_mil / crate::board::MM_PER_MIL;
    let target_px = pixmap.width() as f64 / 6.0;
    let mm = [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0]
        .into_iter()
        .rev()
        .find(|&mm| mm * px_per_mm <= target_px)
        .unwrap_or(0.1);
    let bar_px = (mm * px_per_mm) as f32;
    let y = pixmap.height() as f32 - 14.0 * text_scale as f32;
    let x = 10.0;
    if let Some(r) = tiny_skia::Rect::from_xywh(x, y, bar_px.max(1.0), 3.0) {
        pixmap.fill_rect(r, &paint((230, 230, 230), 255), Transform::identity(), None);
    }
    let label = if mm < 1.0 { format!("{mm} mm") } else { format!("{} mm", mm as u32) };
    draw_text(pixmap, &label, x + bar_px + 6.0, y - 3.0 * text_scale as f32, text_scale, (230, 230, 230));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::raw::*;

    #[test]
    fn renders_png() {
        let raw = RawBoard {
            format: "test".into(),
            outline_points: vec![],
            outline_segments: vec![],
            parts: vec![RawPart {
                name: "C1".into(),
                mfgcode: String::new(),
                side: RawSide::Top,
                part_type: RawPartType::Smd,
                end_of_pins: 2,
                p1: RawPoint::default(),
                p2: RawPoint::default(),
            }],
            pins: vec![
                RawPin {
                    pos: RawPoint { x: 0, y: 0 },
                    probe: 0,
                    part: 1,
                    side: RawSide::Top,
                    radius: 10.0,
                    net: "PP3V3".into(),
                    number: None,
                    name: None,
                },
                RawPin {
                    pos: RawPoint { x: 100, y: 0 },
                    probe: 0,
                    part: 1,
                    side: RawSide::Top,
                    radius: 10.0,
                    net: "GND".into(),
                    number: None,
                    name: None,
                },
            ],
            nails: vec![],
        };
        let board = Board::from_raw("/tmp/t.brd".into(), raw);
        let out = render(
            &board,
            &RenderRequest {
                side: Side::Top,
                width: 400,
                highlight_parts: vec![0],
                highlight_nets: vec![board.net("PP3V3").unwrap()],
                viewport: None,
                label_all: false,
            },
        )
        .unwrap();
        assert_eq!(&out.png[1..4], b"PNG");
        assert_eq!(out.width, 400);
        assert_eq!(out.legend[0].0, "PP3V3");
    }
}
