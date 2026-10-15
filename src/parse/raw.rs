//! Parser output as produced by OpenBoardView, before any interpretation.
//!
//! These types mirror `BRDFileBase` field for field. They cross the process
//! boundary between the parse worker and the server as JSON.

use serde::{Deserialize, Serialize};

/// Side of the board, as stored by the parsers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RawSide {
    Both,
    Bottom,
    Top,
}

impl RawSide {
    pub fn from_ffi(value: u8) -> Self {
        match value {
            1 => RawSide::Bottom,
            2 => RawSide::Top,
            _ => RawSide::Both,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawPartType {
    Smd,
    ThroughHole,
}

/// A point in mils.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RawPoint {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawPart {
    pub name: String,
    pub mfgcode: String,
    pub side: RawSide,
    pub part_type: RawPartType,
    pub end_of_pins: u32,
    pub p1: RawPoint,
    pub p2: RawPoint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawPin {
    pub pos: RawPoint,
    pub probe: i32,
    /// 1-based index into [`RawBoard::parts`].
    pub part: u32,
    pub side: RawSide,
    pub radius: f64,
    pub net: String,
    pub number: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawNail {
    pub probe: u32,
    pub pos: RawPoint,
    pub side: RawSide,
    pub net: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawBoard {
    /// Short format identifier, e.g. `brd`, `fz`, `gencad`.
    pub format: String,
    pub outline_points: Vec<RawPoint>,
    pub outline_segments: Vec<(RawPoint, RawPoint)>,
    pub parts: Vec<RawPart>,
    pub pins: Vec<RawPin>,
    pub nails: Vec<RawNail>,
}
