//! Serializable views of the board model returned by the tools.
//!
//! Coordinates are in mils (1 mil = 0.0254 mm), OpenBoardView's native unit,
//! rounded to whole mils.

use schemars::JsonSchema;
use serde::Serialize;

use crate::board::{Board, MountType, NetKind, PartKind, PinKind, Side};

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct XY {
    pub x: i64,
    pub y: i64,
}

impl XY {
    pub fn from(p: crate::board::Point) -> Self {
        Self { x: p.x.round() as i64, y: p.y.round() as i64 }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PartSummary {
    pub name: String,
    pub kind: PartKind,
    pub side: Side,
    pub mount: MountType,
    /// Part number or value from the board file, when present.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mfgcode: String,
    pub pin_count: usize,
    pub center: XY,
}

impl PartSummary {
    pub fn new(board: &Board, part: usize) -> Self {
        let p = &board.parts[part];
        Self {
            name: p.name.clone(),
            kind: p.kind,
            side: p.side,
            mount: p.mount,
            mfgcode: p.mfgcode.clone(),
            pin_count: p.pins.len(),
            center: XY::from(p.center()),
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PinInfo {
    pub number: String,
    /// Pad name (e.g. BGA ball `AY12`); omitted when equal to the number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub net: String,
    pub net_kind: NetKind,
    pub kind: PinKind,
    pub side: Side,
    pub position: XY,
}

impl PinInfo {
    pub fn new(board: &Board, pin: usize) -> Self {
        let p = &board.pins[pin];
        let net = &board.nets[p.net];
        Self {
            number: p.number.clone(),
            name: (p.name != p.number).then(|| p.name.clone()),
            net: net.name.clone(),
            net_kind: net.kind,
            kind: p.kind,
            side: p.side,
            position: XY::from(p.pos),
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NetSummary {
    pub name: String,
    pub kind: NetKind,
    pub pin_count: usize,
    pub part_count: usize,
    /// Test pads plus test-point parts on the net.
    pub test_point_count: usize,
    /// Probe number assigned by the board file, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<i64>,
}

impl NetSummary {
    pub fn new(board: &Board, net: usize) -> Self {
        let n = &board.nets[net];
        Self {
            name: n.name.clone(),
            kind: n.kind,
            pin_count: n.pins.len(),
            part_count: board.net_parts(net).len(),
            test_point_count: n
                .pins
                .iter()
                .filter(|&&p| {
                    let pin = &board.pins[p];
                    pin.kind == PinKind::TestPad || pin.part.is_some_and(|i| board.parts[i].kind == PartKind::TestPoint)
                })
                .count(),
            probe: n.number,
        }
    }
}
