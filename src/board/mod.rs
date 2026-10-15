//! Interpreted board model.
//!
//! [`Board::from_raw`] is a port of OpenBoardView's `BRDBoard` constructor and
//! the outline fallback in `BoardView::LoadBoard`, so parts, pins and nets come
//! out the way the GUI shows them: test pads grouped under one virtual part,
//! every `UNCONNECTED*` net folded into a single `UNCONNECTED` net, pin numbers
//! synthesized when the format has none.

pub mod classify;

use std::collections::HashMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::parse::raw::{RawBoard, RawPartType, RawPoint, RawSide};
pub use classify::{NetKind, PartKind, classify_net, classify_part, refine_part_kind};

/// Name OpenBoardView gives the virtual part that owns test pads.
pub const TEST_PAD_PART: &str = "...";
pub const UNCONNECTED_NET: &str = "UNCONNECTED";
/// Margin, in mils, around the pins when a file has no outline (as in OBV).
const GENERATED_OUTLINE_MARGIN: i32 = 200;

pub const MM_PER_MIL: f64 = 0.0254;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Top,
    Bottom,
    Both,
}

impl Side {
    fn from_raw(side: RawSide) -> Self {
        match side {
            RawSide::Top => Side::Top,
            RawSide::Bottom => Side::Bottom,
            RawSide::Both => Side::Both,
        }
    }

    /// True if an element on `self` is visible when looking at `view`.
    pub fn visible_from(self, view: Side) -> bool {
        self == Side::Both || view == Side::Both || self == view
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MountType {
    Smd,
    ThroughHole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PinKind {
    Component,
    TestPad,
    NotConnected,
}

/// A position in mils.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    fn from_raw(p: RawPoint) -> Self {
        Self::new(p.x as f64, p.y as f64)
    }
}

/// Axis-aligned rectangle in mils.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Rect {
    pub min: Point,
    pub max: Point,
}

impl Rect {
    pub fn empty() -> Self {
        Self { min: Point::new(f64::INFINITY, f64::INFINITY), max: Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY) }
    }

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y
    }

    pub fn include(&mut self, p: Point) {
        self.min.x = self.min.x.min(p.x);
        self.min.y = self.min.y.min(p.y);
        self.max.x = self.max.x.max(p.x);
        self.max.y = self.max.y.max(p.y);
    }

    pub fn include_rect(&mut self, other: &Rect) {
        if !other.is_empty() {
            self.include(other.min);
            self.include(other.max);
        }
    }

    pub fn expanded(&self, by: f64) -> Rect {
        Rect { min: Point::new(self.min.x - by, self.min.y - by), max: Point::new(self.max.x + by, self.max.y + by) }
    }

    pub fn center(&self) -> Point {
        Point::new((self.min.x + self.max.x) / 2.0, (self.min.y + self.max.y) / 2.0)
    }

    pub fn width(&self) -> f64 {
        (self.max.x - self.min.x).max(0.0)
    }

    pub fn height(&self) -> f64 {
        (self.max.y - self.min.y).max(0.0)
    }
}

#[derive(Debug, Clone)]
pub struct Part {
    pub name: String,
    pub mfgcode: String,
    pub side: Side,
    pub mount: MountType,
    pub kind: PartKind,
    pub pins: Vec<usize>,
    /// Bounding box of the part's pins (including pad radius).
    pub bbox: Rect,
}

impl Part {
    pub fn center(&self) -> Point {
        self.bbox.center()
    }
}

#[derive(Debug, Clone)]
pub struct Pin {
    /// Owning part, or `None` for test pads.
    pub part: Option<usize>,
    pub number: String,
    pub name: String,
    pub pos: Point,
    pub side: Side,
    pub net: usize,
    pub radius: f64,
    pub probe: i32,
    pub kind: PinKind,
}

#[derive(Debug, Clone)]
pub struct Net {
    pub name: String,
    pub kind: NetKind,
    /// Probe/nail number when the file assigns one.
    pub number: Option<i64>,
    pub pins: Vec<usize>,
    pub nails: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct Nail {
    pub probe: u32,
    pub pos: Point,
    pub side: Side,
}

#[derive(Debug, Clone)]
pub struct Board {
    pub path: PathBuf,
    pub format: String,
    /// Real parts, sorted by name. Test pads are not parts; see [`Pin::part`].
    pub parts: Vec<Part>,
    pub pins: Vec<Pin>,
    pub nets: Vec<Net>,
    pub nails: Vec<Nail>,
    /// Closed polyline outline (may be generated from the pin extents).
    pub outline_points: Vec<Point>,
    pub outline_segments: Vec<(Point, Point)>,
    pub outline_generated: bool,
    pub bbox: Rect,
    part_index: HashMap<String, usize>,
    net_index: HashMap<String, usize>,
}

fn is_unconnected(net: &str) -> bool {
    net.is_empty() || net.starts_with(UNCONNECTED_NET)
}

impl Board {
    pub fn from_raw(path: PathBuf, raw: RawBoard) -> Self {
        let mut nets: Vec<Net> = Vec::new();
        let mut net_index: HashMap<String, usize> = HashMap::new();

        let mut intern_net = |nets: &mut Vec<Net>, name: &str, number: Option<i64>| -> usize {
            if let Some(&idx) = net_index.get(name) {
                if nets[idx].number.is_none() {
                    nets[idx].number = number;
                }
                return idx;
            }
            nets.push(Net {
                name: name.to_string(),
                kind: classify_net(name),
                number,
                pins: Vec::new(),
                nails: Vec::new(),
            });
            net_index.insert(name.to_string(), nets.len() - 1);
            nets.len() - 1
        };

        let unconnected = intern_net(&mut nets, UNCONNECTED_NET, None);
        // Nails first, as BRDBoard does, so nets pick up their probe numbers.
        let mut nails = Vec::with_capacity(raw.nails.len());
        for nail in &raw.nails {
            let net = if is_unconnected(&nail.net) {
                unconnected
            } else {
                intern_net(&mut nets, &nail.net, Some(nail.probe as i64))
            };
            nets[net].nails.push(nails.len());
            nails.push(Nail {
                probe: nail.probe,
                pos: Point::from_raw(nail.pos),
                side: if nail.side == RawSide::Top { Side::Top } else { Side::Bottom },
            });
        }

        // Parts, keeping a map from raw (1-based) part index to our index.
        let mut parts: Vec<Part> = Vec::new();
        let mut raw_to_part: Vec<Option<usize>> = Vec::with_capacity(raw.parts.len());
        for part in &raw.parts {
            if part.name.starts_with(TEST_PAD_PART) {
                raw_to_part.push(None);
                continue;
            }
            raw_to_part.push(Some(parts.len()));
            let mut bbox = Rect::empty();
            if part.p1 != part.p2 {
                bbox.include(Point::from_raw(part.p1));
                bbox.include(Point::from_raw(part.p2));
            }
            parts.push(Part {
                name: part.name.clone(),
                mfgcode: part.mfgcode.trim().to_string(),
                side: Side::from_raw(part.side),
                mount: match part.part_type {
                    RawPartType::Smd => MountType::Smd,
                    RawPartType::ThroughHole => MountType::ThroughHole,
                },
                kind: classify_part(&part.name),
                pins: Vec::new(),
                bbox,
            });
        }

        let mut pins = Vec::with_capacity(raw.pins.len());
        let mut pin_idx = 0u32;
        let mut part_idx = 1u32;
        let mut part_has_pin_bbox = vec![false; parts.len()];
        for raw_pin in &raw.pins {
            // Pins pointing past the part list are dropped, like OBV's null check.
            let Some(raw_part) = (raw_pin.part as usize).checked_sub(1).and_then(|i| raw_to_part.get(i)) else {
                continue;
            };

            pin_idx += 1;
            if raw_pin.part != part_idx {
                part_idx = raw_pin.part;
                pin_idx = 1;
            }
            let number = raw_pin.number.clone().unwrap_or_else(|| pin_idx.to_string());
            let name = raw_pin.name.clone().unwrap_or_else(|| number.clone());

            let not_connected = is_unconnected(&raw_pin.net);
            let net = if not_connected { unconnected } else { intern_net(&mut nets, &raw_pin.net, None) };
            let kind = match raw_part {
                None => PinKind::TestPad,
                Some(_) if not_connected => PinKind::NotConnected,
                Some(_) => PinKind::Component,
            };

            let pos = Point::from_raw(raw_pin.pos);
            let idx = pins.len();
            if let Some(p) = *raw_part {
                let part = &mut parts[p];
                if !part_has_pin_bbox[p] {
                    part.bbox = Rect::empty();
                    part_has_pin_bbox[p] = true;
                }
                let r = raw_pin.radius.max(0.0);
                part.bbox.include(Point::new(pos.x - r, pos.y - r));
                part.bbox.include(Point::new(pos.x + r, pos.y + r));
                part.pins.push(idx);
            }
            nets[net].pins.push(idx);
            pins.push(Pin {
                part: *raw_part,
                number,
                name,
                pos,
                side: Side::from_raw(raw_pin.side),
                net,
                radius: raw_pin.radius,
                probe: raw_pin.probe,
                kind,
            });
        }

        // Sort parts by name (as OBV does) and remap pin -> part references.
        let mut order: Vec<usize> = (0..parts.len()).collect();
        order.sort_by(|&a, &b| parts[a].name.cmp(&parts[b].name));
        let mut new_index = vec![0usize; parts.len()];
        for (new, &old) in order.iter().enumerate() {
            new_index[old] = new;
        }
        for part in &mut parts {
            part.kind = refine_part_kind(part.kind, part.pins.len());
        }
        let mut slots: Vec<Option<Part>> = parts.into_iter().map(Some).collect();
        let parts: Vec<Part> = order.iter().map(|&old| slots[old].take().unwrap()).collect();
        for pin in &mut pins {
            if let Some(p) = pin.part.as_mut() {
                *p = new_index[*p];
            }
        }

        let mut outline_points: Vec<Point> = raw.outline_points.iter().copied().map(Point::from_raw).collect();
        let outline_segments: Vec<(Point, Point)> =
            raw.outline_segments.iter().map(|(a, b)| (Point::from_raw(*a), Point::from_raw(*b))).collect();

        let mut outline_generated = false;
        if outline_points.len() < 3 && outline_segments.len() < 3 && !pins.is_empty() {
            let mut r = Rect::empty();
            for pin in &pins {
                r.include(pin.pos);
            }
            let r = r.expanded(GENERATED_OUTLINE_MARGIN as f64);
            outline_points = vec![r.min, Point::new(r.max.x, r.min.y), r.max, Point::new(r.min.x, r.max.y), r.min];
            outline_generated = true;
        }

        let mut bbox = Rect::empty();
        for p in &outline_points {
            bbox.include(*p);
        }
        for (a, b) in &outline_segments {
            bbox.include(*a);
            bbox.include(*b);
        }
        if bbox.is_empty() {
            for pin in &pins {
                bbox.include(pin.pos);
            }
        }

        let part_index = parts.iter().enumerate().map(|(i, p)| (p.name.to_ascii_uppercase(), i)).collect();
        let net_index = nets.iter().enumerate().map(|(i, n)| (n.name.to_ascii_uppercase(), i)).collect();

        Board {
            path,
            format: raw.format,
            parts,
            pins,
            nets,
            nails,
            outline_points,
            outline_segments,
            outline_generated,
            bbox,
            part_index,
            net_index,
        }
    }

    /// Looks up a part by name, case-insensitively.
    pub fn part(&self, name: &str) -> Option<usize> {
        self.part_index.get(&name.trim().to_ascii_uppercase()).copied()
    }

    /// Looks up a net by name, case-insensitively.
    pub fn net(&self, name: &str) -> Option<usize> {
        self.net_index.get(&name.trim().to_ascii_uppercase()).copied()
    }

    /// Finds a pin of `part` by number or by pad name (e.g. `1` or `AY12`).
    pub fn part_pin(&self, part: usize, pin: &str) -> Option<usize> {
        let pin = pin.trim();
        let pins = &self.parts[part].pins;
        pins.iter()
            .copied()
            .find(|&i| self.pins[i].number.eq_ignore_ascii_case(pin))
            .or_else(|| pins.iter().copied().find(|&i| self.pins[i].name.eq_ignore_ascii_case(pin)))
    }

    /// Distinct nets touched by a part, in pin order.
    pub fn part_nets(&self, part: usize) -> Vec<usize> {
        let mut nets = Vec::new();
        for &pin in &self.parts[part].pins {
            let net = self.pins[pin].net;
            if !nets.contains(&net) {
                nets.push(net);
            }
        }
        nets
    }

    /// Distinct real parts on a net.
    pub fn net_parts(&self, net: usize) -> Vec<usize> {
        let mut parts = Vec::new();
        for &pin in &self.nets[net].pins {
            if let Some(part) = self.pins[pin].part
                && !parts.contains(&part)
            {
                parts.push(part);
            }
        }
        parts
    }

    pub fn test_pad_count(&self) -> usize {
        self.pins.iter().filter(|p| p.kind == PinKind::TestPad).count()
    }

    pub fn is_ground(&self, net: usize) -> bool {
        self.nets[net].kind == NetKind::Ground
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::raw::*;

    fn pin(x: i32, part: u32, net: &str) -> RawPin {
        RawPin {
            pos: RawPoint { x, y: 0 },
            probe: 0,
            part,
            side: RawSide::Top,
            radius: 5.0,
            net: net.to_string(),
            number: None,
            name: None,
        }
    }

    fn part(name: &str) -> RawPart {
        RawPart {
            name: name.to_string(),
            mfgcode: String::new(),
            side: RawSide::Top,
            part_type: RawPartType::Smd,
            end_of_pins: 0,
            p1: RawPoint::default(),
            p2: RawPoint::default(),
        }
    }

    pub fn sample() -> Board {
        let raw = RawBoard {
            format: "test".into(),
            outline_points: vec![],
            outline_segments: vec![],
            parts: vec![part("R2"), part("C1"), part("..."), part("U1")],
            pins: vec![
                pin(0, 1, "PP3V3"),
                pin(10, 1, "EN"),
                pin(20, 2, "PP3V3"),
                pin(30, 2, "GND"),
                pin(40, 3, "PP3V3"),
                pin(50, 4, "EN"),
                pin(60, 4, "UNCONNECTED_7"),
                pin(70, 4, ""),
            ],
            nails: vec![RawNail {
                probe: 12,
                pos: RawPoint { x: 1, y: 1 },
                side: RawSide::Bottom,
                net: "PP3V3".into(),
            }],
        };
        Board::from_raw(PathBuf::from("/tmp/sample.brd"), raw)
    }

    #[test]
    fn builds_like_openboardview() {
        let b = sample();
        // Test pad part removed, remaining parts sorted by name.
        let names: Vec<&str> = b.parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["C1", "R2", "U1"]);

        let r2 = b.part("r2").unwrap();
        assert_eq!(b.parts[r2].pins.len(), 2);
        assert_eq!(b.pins[b.parts[r2].pins[1]].number, "2");

        let pp3v3 = b.net("PP3V3").unwrap();
        assert_eq!(b.nets[pp3v3].number, Some(12));
        assert_eq!(b.nets[pp3v3].pins.len(), 3);
        assert_eq!(b.nets[pp3v3].kind, NetKind::Power);
        assert_eq!(b.test_pad_count(), 1);

        let nc = b.net(UNCONNECTED_NET).unwrap();
        assert_eq!(b.nets[nc].pins.len(), 2);
        let u1 = b.part("U1").unwrap();
        assert!(b.parts[u1].pins.iter().skip(1).all(|&p| b.pins[p].kind == PinKind::NotConnected));

        assert!(b.outline_generated);
        assert_eq!(b.outline_points.len(), 5);
        assert_eq!(b.net_parts(pp3v3).len(), 2);
    }

    #[test]
    fn part_pin_lookup() {
        let b = sample();
        let c1 = b.part("C1").unwrap();
        let pin = b.part_pin(c1, "2").unwrap();
        assert_eq!(b.nets[b.pins[pin].net].name, "GND");
    }
}
