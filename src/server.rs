//! MCP server: tool definitions and their glue to the board model.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{Json, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::analysis::{self, BoardDiff, NameFilter, SearchHit};
use crate::annotations::{self, Annotation, NewAnnotation};
use crate::board::{Board, MM_PER_MIL, NetKind, PartKind, PinKind, Point, Rect, Side};
use crate::dto::{NetSummary, PartSummary, PinInfo, XY};
use crate::render::{self, RenderRequest};
use crate::schematic::{self, PageSummary, SchematicCache, SchematicHit, SchematicLocation};
use crate::store::{BoardStore, LoadedBoard, resolve_path};

type ToolResult<T> = Result<Json<T>, String>;

const INSTRUCTIONS: &str = "\
Inspect PCB boardview files (the formats OpenBoardView reads: .brd, .bdv, .bvr, .cad, .cst, .fz, .cae, \
GenCAD, Altium ASCII, XZZPCB .pcb, ASC). Call open_board first; later calls may omit `path` to use the \
most recently opened board. Coordinates are in mils (1 mil = 0.0254 mm); Y grows upward, and the bottom \
side is shown mirrored, as in OpenBoardView. Part and net kinds are guesses from reference designators and \
net names, not data from the file. Typical repair flow for a shorted rail: power_rails, then \
parts_between_nets(rail) to list capacitors to ground, then nearby_parts / render_board to locate them. \
Annotations are shared with the OpenBoardView GUI (reopen the board there to see new ones).";

#[derive(Clone)]
pub struct ObvServer {
    store: Arc<BoardStore>,
    schematics: Arc<SchematicCache>,
    viewer: String,
    tool_router: ToolRouter<Self>,
}

impl ObvServer {
    pub fn new(store: BoardStore, viewer: String) -> Self {
        Self {
            store: Arc::new(store),
            schematics: Arc::new(SchematicCache::default()),
            viewer,
            tool_router: Self::tool_router(),
        }
    }

    async fn board(&self, path: Option<&str>) -> Result<Arc<LoadedBoard>, String> {
        self.store.get(path).await.map_err(|e| format!("{e:#}"))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn find_part(board: &Board, name: &str) -> Result<usize, String> {
    board.part(name).ok_or_else(|| {
        let s = analysis::suggest(board.parts.iter().map(|p| p.name.as_str()), name, 5);
        not_found("part", name, &s)
    })
}

fn find_net(board: &Board, name: &str) -> Result<usize, String> {
    board.net(name).ok_or_else(|| {
        let s = analysis::suggest(board.nets.iter().map(|n| n.name.as_str()), name, 5);
        not_found("net", name, &s)
    })
}

fn not_found(what: &str, name: &str, suggestions: &[String]) -> String {
    if suggestions.is_empty() {
        format!("no {what} named '{name}' on this board")
    } else {
        format!("no {what} named '{name}' on this board; did you mean: {}?", suggestions.join(", "))
    }
}

fn mm(mils: f64) -> f64 {
    (mils * MM_PER_MIL * 100.0).round() / 100.0
}

fn clamp_limit(limit: Option<usize>, default: usize, max: usize) -> usize {
    limit.unwrap_or(default).clamp(1, max)
}

fn pin_labels(board: &Board, pins: &[usize]) -> Vec<String> {
    pins.iter()
        .map(|&p| {
            let pin = &board.pins[p];
            if pin.name != pin.number { format!("{} ({})", pin.number, pin.name) } else { pin.number.clone() }
        })
        .collect()
}

fn names(board: &Board, parts: &[usize]) -> Vec<String> {
    parts.iter().map(|&p| board.parts[p].name.clone()).collect()
}

fn path_string(p: &Path) -> String {
    p.display().to_string()
}

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenBoardParams {
    /// Path to the boardview file. `~` is expanded.
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BoardParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListPartsParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
    /// Name filter: glob with `*`/`?` (e.g. `PC*`), otherwise substring. Case-insensitive.
    pub filter: Option<String>,
    /// Only parts of this kind.
    pub kind: Option<PartKind>,
    /// Only parts mounted on this side.
    pub side: Option<Side>,
    /// Only parts whose part number/value contains this text.
    pub mfgcode: Option<String>,
    pub offset: Option<usize>,
    /// Maximum results (default 100, max 1000).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetPartParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
    /// Reference designator, e.g. `U7000` or `PC123`.
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListNetsParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
    /// Name filter: glob with `*`/`?` (e.g. `PP3V3*`), otherwise substring. Case-insensitive.
    pub filter: Option<String>,
    /// Only nets of this kind.
    pub kind: Option<NetKind>,
    /// Only nets with at least this many pins.
    pub min_pins: Option<usize>,
    /// `name` (default) or `pins` (most pins first).
    pub sort: Option<NetSort>,
    pub offset: Option<usize>,
    /// Maximum results (default 100, max 1000).
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NetSort {
    Name,
    Pins,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetNetParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
    /// Net name, e.g. `PP3V3_S5` or `+3VS`.
    pub name: String,
    /// Maximum parts listed (default 200, max 2000).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
    /// Text to find in part names, net names and part numbers. Globs (`*`, `?`) are supported; plain text is fuzzy-ranked.
    pub query: String,
    /// Maximum results (default 25, max 200).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BetweenParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
    /// First net.
    pub net_a: String,
    /// Second net. Omit to use every ground net (the usual short-to-ground check).
    pub net_b: Option<String>,
    /// Only parts of this kind (e.g. `capacitor`).
    pub kind: Option<PartKind>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TraceParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
    /// Net to start from.
    pub net: String,
    /// How many hops to follow (default 2, max 6).
    pub depth: Option<usize>,
    /// Part kinds to cross. Default: resistor, inductor, fuse, diode, jumper.
    pub through: Option<Vec<PartKind>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NearbyParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
    /// Center on this part. Alternatively give `x` and `y`.
    pub part: Option<String>,
    /// Center X in mils.
    pub x: Option<f64>,
    /// Center Y in mils.
    pub y: Option<f64>,
    /// Search radius in millimetres (default 3).
    pub radius_mm: Option<f64>,
    /// Side to search. Defaults to the side of `part`, or both.
    pub side: Option<Side>,
    /// Maximum results (default 50, max 500).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RailsParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
    /// Name filter (glob or substring).
    pub filter: Option<String>,
    /// Maximum results (default 50, max 500).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RenderParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
    /// Side to view (default `top`). The bottom view is mirrored like flipping the board.
    pub side: Option<Side>,
    /// Parts to highlight (outlined in red and labeled).
    #[serde(default)]
    pub highlight_parts: Vec<String>,
    /// Nets whose pins to highlight, each in its own color.
    #[serde(default)]
    pub highlight_nets: Vec<String>,
    /// Zoom to these parts and/or nets. Default: highlighted parts if any, else the whole board.
    #[serde(default)]
    pub focus: Vec<String>,
    /// Extra margin around the focus area, in millimetres (default 3).
    pub margin_mm: Option<f64>,
    /// Image width in pixels (default 1200, 256..2048).
    pub width: Option<u32>,
    /// Label every visible part, even in a crowded view.
    #[serde(default)]
    pub label_all: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddAnnotationParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
    /// The note text.
    pub note: String,
    /// Part the note is about. Positions the note on the part.
    pub part: Option<String>,
    /// Pin of `part` (number or pad name). Positions the note on the pin.
    pub pin: Option<String>,
    /// Net the note is about. Filled in automatically from `pin`.
    pub net: Option<String>,
    /// Explicit X position in mils (when no part is given).
    pub x: Option<f64>,
    /// Explicit Y position in mils.
    pub y: Option<f64>,
    /// Side the note appears on (default: the part's side, else top).
    pub side: Option<Side>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateAnnotationParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
    pub id: i64,
    /// Replacement note text.
    pub note: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteAnnotationParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
    pub id: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LinkSchematicParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
    /// Schematic PDF to associate with the board.
    pub pdf: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchSchematicParams {
    /// Board file path. Defaults to the most recently opened board.
    pub path: Option<String>,
    /// Text to find, e.g. a reference designator or net name.
    pub query: String,
    /// Search this PDF instead of the one linked to the board.
    pub pdf: Option<String>,
    /// Require whole-identifier matches so `C12` does not match `C123` (default true).
    pub whole_word: Option<bool>,
    /// Maximum matching lines returned (default 40, max 500).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CompareParams {
    /// First board (e.g. the older revision).
    pub path_a: String,
    /// Second board.
    pub path_b: String,
    /// Maximum entries per list (default 200, max 5000).
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, JsonSchema)]
pub struct SideCounts {
    pub top: usize,
    pub bottom: usize,
    pub both: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct BoardSummary {
    pub path: String,
    pub format: String,
    pub units: &'static str,
    pub parts: usize,
    pub pins: usize,
    pub nets: usize,
    pub test_pads: usize,
    pub nails: usize,
    pub width_mm: f64,
    pub height_mm: f64,
    pub bbox_min: XY,
    pub bbox_max: XY,
    /// True when the file had no outline and one was generated around the pins.
    pub outline_generated: bool,
    pub parts_by_kind: BTreeMap<String, usize>,
    pub parts_by_side: SideCounts,
    pub ground_nets: Vec<String>,
    /// Largest likely power rails by pin count.
    pub top_power_rails: Vec<NetSummary>,
    pub annotations: usize,
    pub schematic: SchematicLocation,
    pub parse_ms: u128,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parser_warnings: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PartList {
    pub total: usize,
    pub offset: usize,
    pub parts: Vec<PartSummary>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PartNet {
    pub net: String,
    pub kind: NetKind,
    /// This part's pins on the net.
    pub pins: Vec<String>,
    /// Other parts on the net (omitted for ground and very large nets).
    pub other_parts: Vec<String>,
    pub other_part_count: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PartDetail {
    pub part: PartSummary,
    pub bbox_min: XY,
    pub bbox_max: XY,
    pub size_mm: (f64, f64),
    pub pins: Vec<PinInfo>,
    pub nets: Vec<PartNet>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<Annotation>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct NetList {
    pub total: usize,
    pub offset: usize,
    pub nets: Vec<NetSummary>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct NetPart {
    pub part: String,
    pub kind: PartKind,
    pub side: Side,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mfgcode: String,
    pub pins: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TestPoint {
    /// Test-point part (e.g. `TP12`); absent for bare test pads and nails.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    pub position: XY,
    pub side: Side,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<i64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct NetDetail {
    pub net: NetSummary,
    pub parts_by_kind: BTreeMap<String, usize>,
    pub parts: Vec<NetPart>,
    pub parts_truncated: bool,
    pub test_points: Vec<TestPoint>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<Annotation>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct BridgePart {
    pub part: PartSummary,
    pub pins_on_a: Vec<String>,
    pub pins_on_b: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct BridgeResult {
    pub net_a: String,
    pub nets_b: Vec<String>,
    pub count: usize,
    pub by_kind: BTreeMap<String, usize>,
    pub parts: Vec<BridgePart>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TraceStep {
    pub net: String,
    pub kind: NetKind,
    pub depth: usize,
    pub from_net: String,
    pub via_part: String,
    pub via_kind: PartKind,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub via_mfgcode: String,
    pub pin_count: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TraceResult {
    pub start: String,
    pub through: Vec<PartKind>,
    pub reached: Vec<TraceStep>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct NearbyPart {
    pub part: PartSummary,
    /// Distance from the center to the part's body (0 = overlapping).
    pub distance_mm: f64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct NearbyResult {
    pub center: XY,
    pub radius_mm: f64,
    pub side: Side,
    pub total: usize,
    pub parts: Vec<NearbyPart>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Rail {
    pub net: String,
    pub pin_count: usize,
    pub part_count: usize,
    pub test_point_count: usize,
    pub caps_to_ground: usize,
    pub inductors: Vec<String>,
    pub ics: Vec<String>,
    pub transistors: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RailList {
    pub total: usize,
    pub rails: Vec<Rail>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RenderInfo {
    pub width: u32,
    pub height: u32,
    pub side: Side,
    pub viewport_min: XY,
    pub viewport_max: XY,
    pub pixels_per_mm: f64,
    pub labeled_parts: usize,
    /// Highlighted nets and their colors.
    pub legend: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AnnotationList {
    pub database: String,
    pub annotations: Vec<Annotation>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AnnotationAdded {
    pub id: i64,
    pub database: String,
    pub annotation: Annotation,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Done {
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SchematicSearchResult {
    pub pdf: String,
    pub page_count: usize,
    /// Pages with matches and how many on each.
    pub pages: Vec<PageSummary>,
    pub hits: Vec<SchematicHit>,
    pub hits_truncated: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ViewerLaunched {
    pub command: Vec<String>,
    pub pid: Option<u32>,
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[tool_router]
impl ObvServer {
    #[tool(
        description = "Open a boardview file and summarize it: format, counts, size, part kinds, ground nets, \
                       main power rails, annotations and linked schematic. Makes it the default board for later calls.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn open_board(&self, Parameters(p): Parameters<OpenBoardParams>) -> ToolResult<BoardSummary> {
        let loaded = self.board(Some(&p.path)).await?;
        let b = &loaded.board;

        let mut parts_by_kind = BTreeMap::new();
        let mut sides = SideCounts { top: 0, bottom: 0, both: 0 };
        for part in &b.parts {
            *parts_by_kind.entry(kind_name(part.kind)).or_insert(0) += 1;
            match part.side {
                Side::Top => sides.top += 1,
                Side::Bottom => sides.bottom += 1,
                Side::Both => sides.both += 1,
            }
        }
        let ground_nets = analysis::ground_nets(b).into_iter().map(|n| b.nets[n].name.clone()).collect();
        let top_power_rails =
            analysis::power_rails(b).into_iter().take(10).map(|r| NetSummary::new(b, r.net)).collect();

        Ok(Json(BoardSummary {
            path: path_string(&b.path),
            format: b.format.clone(),
            units: "mil",
            parts: b.parts.len(),
            pins: b.pins.len(),
            nets: b.nets.len(),
            test_pads: b.test_pad_count(),
            nails: b.nails.len(),
            width_mm: mm(b.bbox.width()),
            height_mm: mm(b.bbox.height()),
            bbox_min: XY::from(b.bbox.min),
            bbox_max: XY::from(b.bbox.max),
            outline_generated: b.outline_generated,
            parts_by_kind,
            parts_by_side: sides,
            ground_nets,
            top_power_rails,
            annotations: annotations::list(&b.path).map(|a| a.len()).unwrap_or(0),
            schematic: schematic::locate(&b.path),
            parse_ms: loaded.parse_time.as_millis(),
            parser_warnings: loaded.warnings.iter().take(10).cloned().collect(),
        }))
    }

    #[tool(
        description = "List parts, filtered by name pattern, kind, side or part number. Paginated.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_parts(&self, Parameters(p): Parameters<ListPartsParams>) -> ToolResult<PartList> {
        let loaded = self.board(p.path.as_deref()).await?;
        let b = &loaded.board;
        let filter = p.filter.as_deref().map(NameFilter::new);
        let mfg = p.mfgcode.as_deref().map(|m| m.to_ascii_uppercase());
        let matched: Vec<usize> = (0..b.parts.len())
            .filter(|&i| {
                let part = &b.parts[i];
                filter.as_ref().is_none_or(|f| f.matches(&part.name))
                    && p.kind.is_none_or(|k| part.kind == k)
                    && p.side.is_none_or(|s| part.side == s)
                    && mfg.as_ref().is_none_or(|m| part.mfgcode.to_ascii_uppercase().contains(m))
            })
            .collect();
        let offset = p.offset.unwrap_or(0);
        let limit = clamp_limit(p.limit, 100, 1000);
        Ok(Json(PartList {
            total: matched.len(),
            offset,
            parts: matched.iter().skip(offset).take(limit).map(|&i| PartSummary::new(b, i)).collect(),
        }))
    }

    #[tool(
        description = "Details of one part: every pin with its net and position, the nets it connects to with the \
                       other parts on each net, and any annotations on it.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_part(&self, Parameters(p): Parameters<GetPartParams>) -> ToolResult<PartDetail> {
        const MAX_NEIGHBORS: usize = 25;
        let loaded = self.board(p.path.as_deref()).await?;
        let b = &loaded.board;
        let idx = find_part(b, &p.name)?;
        let part = &b.parts[idx];

        let nets = b
            .part_nets(idx)
            .into_iter()
            .map(|net| {
                let own: Vec<usize> = part.pins.iter().copied().filter(|&pin| b.pins[pin].net == net).collect();
                let others: Vec<usize> = b.net_parts(net).into_iter().filter(|&o| o != idx).collect();
                let kind = b.nets[net].kind;
                let list = if matches!(kind, NetKind::Ground | NetKind::Unconnected) || others.len() > MAX_NEIGHBORS {
                    Vec::new()
                } else {
                    names(b, &others)
                };
                PartNet {
                    net: b.nets[net].name.clone(),
                    kind,
                    pins: pin_labels(b, &own),
                    other_parts: list,
                    other_part_count: others.len(),
                }
            })
            .collect();

        let notes = annotations::list(&b.path)
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.part.eq_ignore_ascii_case(&part.name))
            .collect();

        Ok(Json(PartDetail {
            part: PartSummary::new(b, idx),
            bbox_min: XY::from(part.bbox.min),
            bbox_max: XY::from(part.bbox.max),
            size_mm: (mm(part.bbox.width()), mm(part.bbox.height())),
            pins: part.pins.iter().map(|&pin| PinInfo::new(b, pin)).collect(),
            nets,
            annotations: notes,
        }))
    }

    #[tool(
        description = "List nets, filtered by name pattern, kind (ground/power/signal) or minimum pin count. \
                       Sort by name or by pin count. Paginated.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_nets(&self, Parameters(p): Parameters<ListNetsParams>) -> ToolResult<NetList> {
        let loaded = self.board(p.path.as_deref()).await?;
        let b = &loaded.board;
        let filter = p.filter.as_deref().map(NameFilter::new);
        let mut matched: Vec<usize> = (0..b.nets.len())
            .filter(|&i| {
                let net = &b.nets[i];
                filter.as_ref().is_none_or(|f| f.matches(&net.name))
                    && p.kind.is_none_or(|k| net.kind == k)
                    && p.min_pins.is_none_or(|m| net.pins.len() >= m)
            })
            .collect();
        match p.sort.unwrap_or(NetSort::Name) {
            NetSort::Name => matched.sort_by(|&a, &c| b.nets[a].name.cmp(&b.nets[c].name)),
            NetSort::Pins => matched.sort_by(|&a, &c| b.nets[c].pins.len().cmp(&b.nets[a].pins.len())),
        }
        let offset = p.offset.unwrap_or(0);
        let limit = clamp_limit(p.limit, 100, 1000);
        Ok(Json(NetList {
            total: matched.len(),
            offset,
            nets: matched.iter().skip(offset).take(limit).map(|&i| NetSummary::new(b, i)).collect(),
        }))
    }

    #[tool(
        description = "Details of one net: every part and pin on it, counts by part kind, test points (test pads, \
                       nails and test-point parts) and annotations.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_net(&self, Parameters(p): Parameters<GetNetParams>) -> ToolResult<NetDetail> {
        let loaded = self.board(p.path.as_deref()).await?;
        let b = &loaded.board;
        let idx = find_net(b, &p.name)?;
        let net = &b.nets[idx];
        let limit = clamp_limit(p.limit, 200, 2000);

        let parts = b.net_parts(idx);
        let mut parts_by_kind = BTreeMap::new();
        for &part in &parts {
            *parts_by_kind.entry(kind_name(b.parts[part].kind)).or_insert(0) += 1;
        }
        let listed = parts
            .iter()
            .take(limit)
            .map(|&part| {
                let pins: Vec<usize> =
                    b.parts[part].pins.iter().copied().filter(|&pin| b.pins[pin].net == idx).collect();
                NetPart {
                    part: b.parts[part].name.clone(),
                    kind: b.parts[part].kind,
                    side: b.parts[part].side,
                    mfgcode: b.parts[part].mfgcode.clone(),
                    pins: pin_labels(b, &pins),
                }
            })
            .collect();

        let mut test_points: Vec<TestPoint> = net
            .pins
            .iter()
            .filter(|&&pin| b.pins[pin].kind == PinKind::TestPad)
            .map(|&pin| TestPoint {
                part: None,
                position: XY::from(b.pins[pin].pos),
                side: b.pins[pin].side,
                probe: (b.pins[pin].probe > 0).then_some(b.pins[pin].probe as i64),
            })
            .collect();
        // Test-point parts (TP12, VT8, ...).
        for &part in &parts {
            if b.parts[part].kind == PartKind::TestPoint {
                let pin = b.parts[part].pins.iter().copied().find(|&pin| b.pins[pin].net == idx);
                test_points.push(TestPoint {
                    part: Some(b.parts[part].name.clone()),
                    position: XY::from(pin.map_or(b.parts[part].center(), |pin| b.pins[pin].pos)),
                    side: b.parts[part].side,
                    probe: None,
                });
            }
        }
        // Nails that were not already turned into test-pad pins by the parser.
        for &nail in &net.nails {
            let n = &b.nails[nail];
            if !test_points.iter().any(|t| t.position.x == n.pos.x as i64 && t.position.y == n.pos.y as i64) {
                test_points.push(TestPoint {
                    part: None,
                    position: XY::from(n.pos),
                    side: n.side,
                    probe: Some(n.probe as i64),
                });
            }
        }

        let notes = annotations::list(&b.path)
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.net.eq_ignore_ascii_case(&net.name))
            .collect();

        Ok(Json(NetDetail {
            net: NetSummary::new(b, idx),
            parts_by_kind,
            parts_truncated: parts.len() > limit,
            parts: listed,
            test_points,
            annotations: notes,
        }))
    }

    #[tool(
        description = "Search part names, net names and part numbers. Supports globs; plain text is ranked by \
                       exact/prefix/substring match and tolerates typos.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn search(&self, Parameters(p): Parameters<SearchParams>) -> ToolResult<SearchResult> {
        let loaded = self.board(p.path.as_deref()).await?;
        Ok(Json(SearchResult { hits: analysis::search(&loaded.board, &p.query, clamp_limit(p.limit, 25, 200)) }))
    }

    #[tool(
        description = "Parts with pins on both of two nets. With only net_a, lists parts between that net and \
                       ground: the capacitors and other parts that can short a rail to ground.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn parts_between_nets(&self, Parameters(p): Parameters<BetweenParams>) -> ToolResult<BridgeResult> {
        let loaded = self.board(p.path.as_deref()).await?;
        let b = &loaded.board;
        let a = find_net(b, &p.net_a)?;
        let nets_b = match p.net_b.as_deref() {
            Some(name) => vec![find_net(b, name)?],
            None => {
                let g = analysis::ground_nets(b);
                if g.is_empty() {
                    return Err("no ground net was recognized on this board; pass net_b explicitly".into());
                }
                g
            }
        };
        if nets_b.contains(&a) {
            return Err("net_a and net_b are the same net".into());
        }
        let bridges: Vec<_> = analysis::parts_between(b, &[a], &nets_b)
            .into_iter()
            .filter(|br| p.kind.is_none_or(|k| b.parts[br.part].kind == k))
            .collect();
        let mut by_kind = BTreeMap::new();
        for br in &bridges {
            *by_kind.entry(kind_name(b.parts[br.part].kind)).or_insert(0) += 1;
        }
        Ok(Json(BridgeResult {
            net_a: b.nets[a].name.clone(),
            nets_b: nets_b.iter().map(|&n| b.nets[n].name.clone()).collect(),
            count: bridges.len(),
            by_kind,
            parts: bridges
                .iter()
                .map(|br| BridgePart {
                    part: PartSummary::new(b, br.part),
                    pins_on_a: pin_labels(b, &br.pins_a),
                    pins_on_b: pin_labels(b, &br.pins_b),
                })
                .collect(),
        }))
    }

    #[tool(
        description = "Follow a net across two-terminal series parts (by default resistors, inductors, fuses, \
                       diodes and jumpers) to find connected nets, e.g. trace a rail back toward its source or forward to its \
                       loads. Ground is never crossed.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn trace_net(&self, Parameters(p): Parameters<TraceParams>) -> ToolResult<TraceResult> {
        let loaded = self.board(p.path.as_deref()).await?;
        let b = &loaded.board;
        let start = find_net(b, &p.net)?;
        let through = p.through.unwrap_or_else(|| {
            vec![PartKind::Resistor, PartKind::Inductor, PartKind::Fuse, PartKind::Diode, PartKind::Jumper]
        });
        let depth = p.depth.unwrap_or(2).clamp(1, 6);
        let reached = analysis::net_neighbors(b, start, depth, &through)
            .into_iter()
            .map(|s| TraceStep {
                net: b.nets[s.net].name.clone(),
                kind: b.nets[s.net].kind,
                depth: s.depth,
                from_net: b.nets[s.from_net].name.clone(),
                via_part: b.parts[s.via_part].name.clone(),
                via_kind: b.parts[s.via_part].kind,
                via_mfgcode: b.parts[s.via_part].mfgcode.clone(),
                pin_count: b.nets[s.net].pins.len(),
            })
            .collect();
        Ok(Json(TraceResult { start: b.nets[start].name.clone(), through, reached }))
    }

    #[tool(
        description = "Parts physically near a part or a point, closest first. Useful to find what else a hot spot, \
                       liquid damage or a reworked area may have affected.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn nearby_parts(&self, Parameters(p): Parameters<NearbyParams>) -> ToolResult<NearbyResult> {
        let loaded = self.board(p.path.as_deref()).await?;
        let b = &loaded.board;
        let (center, exclude, default_side) = match (&p.part, p.x, p.y) {
            (Some(name), _, _) => {
                let idx = find_part(b, name)?;
                (b.parts[idx].center(), Some(idx), b.parts[idx].side)
            }
            (None, Some(x), Some(y)) => (Point::new(x, y), None, Side::Both),
            _ => return Err("give either `part` or both `x` and `y`".into()),
        };
        let radius_mm = p.radius_mm.unwrap_or(3.0).clamp(0.01, 500.0);
        let side = p.side.unwrap_or(default_side);
        let found = analysis::nearby_parts(b, center, radius_mm / MM_PER_MIL, side, exclude);
        let limit = clamp_limit(p.limit, 50, 500);
        Ok(Json(NearbyResult {
            center: XY::from(center),
            radius_mm,
            side,
            total: found.len(),
            parts: found
                .into_iter()
                .take(limit)
                .map(|(i, d)| NearbyPart { part: PartSummary::new(b, i), distance_mm: mm(d) })
                .collect(),
        }))
    }

    #[tool(
        description = "Likely power rails (by net name), largest first, with how many capacitors decouple each to \
                       ground and which inductors, ICs and transistors sit on it.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn power_rails(&self, Parameters(p): Parameters<RailsParams>) -> ToolResult<RailList> {
        let loaded = self.board(p.path.as_deref()).await?;
        let b = &loaded.board;
        let filter = p.filter.as_deref().map(NameFilter::new);
        let rails: Vec<_> = analysis::power_rails(b)
            .into_iter()
            .filter(|r| filter.as_ref().is_none_or(|f| f.matches(&b.nets[r.net].name)))
            .collect();
        let limit = clamp_limit(p.limit, 50, 500);
        Ok(Json(RailList {
            total: rails.len(),
            rails: rails
                .iter()
                .take(limit)
                .map(|r| {
                    let s = NetSummary::new(b, r.net);
                    Rail {
                        net: s.name,
                        pin_count: s.pin_count,
                        part_count: s.part_count,
                        test_point_count: s.test_point_count,
                        caps_to_ground: r.caps_to_ground.len(),
                        inductors: names(b, &r.inductors),
                        ics: names(b, &r.ics),
                        transistors: names(b, &r.transistors),
                    }
                })
                .collect(),
        }))
    }

    #[tool(
        description = "Render the board (or a zoomed region) to a PNG image, highlighting parts and nets. The bottom \
                       side is mirrored as if the board were flipped. Use focus to zoom in on parts or nets.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn render_board(&self, Parameters(p): Parameters<RenderParams>) -> Result<CallToolResult, String> {
        let loaded = self.board(p.path.as_deref()).await?;
        let b = &loaded.board;

        let highlight_parts = p.highlight_parts.iter().map(|n| find_part(b, n)).collect::<Result<Vec<_>, _>>()?;
        let highlight_nets = p.highlight_nets.iter().map(|n| find_net(b, n)).collect::<Result<Vec<_>, _>>()?;

        let margin = p.margin_mm.unwrap_or(3.0).max(0.0) / MM_PER_MIL;
        let mut area = Rect::empty();
        if p.focus.is_empty() {
            for &part in &highlight_parts {
                area.include_rect(&b.parts[part].bbox);
            }
        } else {
            for name in &p.focus {
                if let Some(part) = b.part(name) {
                    area.include_rect(&b.parts[part].bbox);
                } else if let Some(net) = b.net(name) {
                    for &pin in &b.nets[net].pins {
                        area.include(b.pins[pin].pos);
                    }
                } else {
                    return Err(format!("focus '{name}' is neither a part nor a net on this board"));
                }
            }
        }
        let viewport = (!area.is_empty()).then(|| area.expanded(margin));

        // Default to the side most highlighted parts are on.
        let side = p.side.unwrap_or_else(|| {
            let bottom = highlight_parts.iter().filter(|&&i| b.parts[i].side == Side::Bottom).count();
            if bottom * 2 > highlight_parts.len() && bottom > 0 { Side::Bottom } else { Side::Top }
        });

        let out = render::render(
            b,
            &RenderRequest {
                side,
                width: p.width.unwrap_or(1200),
                highlight_parts,
                highlight_nets,
                viewport,
                label_all: p.label_all,
            },
        )
        .map_err(|e| format!("{e:#}"))?;

        let info = RenderInfo {
            width: out.width,
            height: out.height,
            side,
            viewport_min: XY::from(out.viewport.min),
            viewport_max: XY::from(out.viewport.max),
            pixels_per_mm: ((out.pixels_per_mil / MM_PER_MIL) * 100.0).round() / 100.0,
            labeled_parts: out.labeled_parts,
            legend: out.legend.into_iter().collect(),
        };
        let png = base64::engine::general_purpose::STANDARD.encode(&out.png);
        Ok(CallToolResult::success(vec![
            ContentBlock::image(png, "image/png"),
            ContentBlock::json(&info).map_err(|e| e.message.to_string())?,
        ]))
    }

    #[tool(
        description = "List the board's annotations (shared with the OpenBoardView GUI).",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_annotations(&self, Parameters(p): Parameters<BoardParams>) -> ToolResult<AnnotationList> {
        let loaded = self.board(p.path.as_deref()).await?;
        let path = &loaded.board.path;
        Ok(Json(AnnotationList {
            database: path_string(&annotations::database_path(path)),
            annotations: annotations::list(path).map_err(|e| format!("{e:#}"))?,
        }))
    }

    #[tool(
        description = "Add an annotation to the board, attached to a part, a pin, a net or a position. Stored in \
                       OpenBoardView's annotation database next to the board file, so it appears in the GUI.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn add_annotation(&self, Parameters(p): Parameters<AddAnnotationParams>) -> ToolResult<AnnotationAdded> {
        let loaded = self.board(p.path.as_deref()).await?;
        let b = &loaded.board;
        if p.note.trim().is_empty() {
            return Err("note must not be empty".into());
        }

        let mut part_name = String::new();
        let mut pin_name = String::new();
        let mut net_name = p.net.clone().unwrap_or_default();
        let mut side = p.side;
        let pos = match (&p.part, p.x, p.y) {
            (Some(name), _, _) => {
                let part = find_part(b, name)?;
                part_name = b.parts[part].name.clone();
                if side.is_none() && b.parts[part].side == Side::Bottom {
                    side = Some(Side::Bottom);
                }
                match &p.pin {
                    Some(pin) => {
                        let idx = b.part_pin(part, pin).ok_or_else(|| format!("{part_name} has no pin '{pin}'"))?;
                        pin_name = b.pins[idx].number.clone();
                        if net_name.is_empty() {
                            net_name = b.nets[b.pins[idx].net].name.clone();
                        }
                        b.pins[idx].pos
                    }
                    None => b.parts[part].center(),
                }
            }
            (None, Some(x), Some(y)) => Point::new(x, y),
            (None, _, _) => match p.net.as_deref() {
                Some(net) => {
                    let idx = find_net(b, net)?;
                    net_name = b.nets[idx].name.clone();
                    let first = b.nets[idx].pins.first().ok_or("that net has no pins to anchor the note to")?;
                    b.pins[*first].pos
                }
                None => return Err("give a part (optionally with pin), a net, or x and y".into()),
            },
        };
        if p.pin.is_some() && p.part.is_none() {
            return Err("`pin` requires `part`".into());
        }

        let new = NewAnnotation {
            side: if side == Some(Side::Bottom) { 1 } else { 0 },
            x: pos.x,
            y: pos.y,
            net: net_name,
            part: part_name,
            pin: pin_name,
            note: p.note.trim().to_string(),
        };
        let id = annotations::add(&b.path, &new).map_err(|e| format!("{e:#}"))?;
        Ok(Json(AnnotationAdded {
            id,
            database: path_string(&annotations::database_path(&b.path)),
            annotation: Annotation {
                id,
                side: new.side,
                x: new.x,
                y: new.y,
                net: new.net,
                part: new.part,
                pin: new.pin,
                note: new.note,
            },
        }))
    }

    #[tool(
        description = "Replace the text of an existing annotation.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn update_annotation(&self, Parameters(p): Parameters<UpdateAnnotationParams>) -> ToolResult<Done> {
        let loaded = self.board(p.path.as_deref()).await?;
        annotations::update(&loaded.board.path, p.id, p.note.trim()).map_err(|e| format!("{e:#}"))?;
        Ok(Json(Done { ok: true, message: format!("annotation {} updated", p.id) }))
    }

    #[tool(
        description = "Delete an annotation. Like the GUI, this hides the row rather than erasing it.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn delete_annotation(&self, Parameters(p): Parameters<DeleteAnnotationParams>) -> ToolResult<Done> {
        let loaded = self.board(p.path.as_deref()).await?;
        annotations::delete(&loaded.board.path, p.id).map_err(|e| format!("{e:#}"))?;
        Ok(Json(Done { ok: true, message: format!("annotation {} deleted", p.id) }))
    }

    #[tool(
        description = "Where the board's schematic PDF is expected (from the board's .conf, else <board>.pdf) and \
                       whether it exists.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn schematic_info(&self, Parameters(p): Parameters<BoardParams>) -> ToolResult<SchematicLocation> {
        let loaded = self.board(p.path.as_deref()).await?;
        Ok(Json(schematic::locate(&loaded.board.path)))
    }

    #[tool(
        description = "Associate a schematic PDF with the board by writing PDFFilePath into the board's .conf file, \
                       the same setting OpenBoardView uses.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn link_schematic(&self, Parameters(p): Parameters<LinkSchematicParams>) -> ToolResult<SchematicLocation> {
        let loaded = self.board(p.path.as_deref()).await?;
        let pdf = resolve_path(&p.pdf).map_err(|e| format!("{e:#}"))?;
        schematic::link(&loaded.board.path, &pdf).map_err(|e| format!("{e:#}"))?;
        Ok(Json(schematic::locate(&loaded.board.path)))
    }

    #[tool(
        description = "Search the schematic PDF's text for a part, net or other text, returning matching pages and \
                       lines. Requires poppler's pdftotext.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn search_schematic(
        &self,
        Parameters(p): Parameters<SearchSchematicParams>,
    ) -> ToolResult<SchematicSearchResult> {
        let pdf: PathBuf = match &p.pdf {
            Some(pdf) => resolve_path(pdf).map_err(|e| format!("{e:#}"))?,
            None => {
                let loaded = self.board(p.path.as_deref()).await?;
                let loc = schematic::locate(&loaded.board.path);
                match (loc.pdf, loc.exists) {
                    (Some(pdf), true) => PathBuf::from(pdf),
                    (Some(pdf), false) => {
                        return Err(format!(
                            "no schematic found (looked for {pdf}); pass `pdf`, or link one with link_schematic"
                        ));
                    }
                    (None, _) => return Err("no schematic is linked to this board".into()),
                }
            }
        };
        let pages = self.schematics.pages(&pdf).await.map_err(|e| format!("{e:#}"))?;
        let limit = clamp_limit(p.limit, 40, 500);
        let (hits, summary) = schematic::search(&pages, &p.query, p.whole_word.unwrap_or(true), limit + 1);
        let truncated = hits.len() > limit;
        Ok(Json(SchematicSearchResult {
            pdf: path_string(&pdf),
            page_count: pages.len(),
            pages: summary,
            hits: hits.into_iter().take(limit).collect(),
            hits_truncated: truncated,
        }))
    }

    #[tool(
        description = "Open the board in the OpenBoardView GUI on this machine.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn open_in_viewer(&self, Parameters(p): Parameters<BoardParams>) -> ToolResult<ViewerLaunched> {
        let path = match p.path.as_deref() {
            Some(path) => resolve_path(path).map_err(|e| format!("{e:#}"))?,
            None => {
                self.store.current_path().await.ok_or("no board is open yet: pass `path`, or call open_board first")?
            }
        };
        let child = tokio::process::Command::new(&self.viewer)
            .arg("-i")
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null()) // stdout carries the MCP protocol
            .stderr(std::process::Stdio::null())
            .kill_on_drop(false)
            .spawn()
            .map_err(|e| format!("cannot start '{}': {e} (set --viewer or OBV_VIEWER)", self.viewer))?;
        Ok(Json(ViewerLaunched {
            command: vec![self.viewer.clone(), "-i".into(), path_string(&path)],
            pid: child.id(),
        }))
    }

    #[tool(
        description = "Compare two boards (e.g. revisions): parts and nets present in only one, part number changes, \
                       and pins whose net changed.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn compare_boards(&self, Parameters(p): Parameters<CompareParams>) -> ToolResult<BoardDiff> {
        let a = self.board(Some(&p.path_a)).await?;
        let b = self.board(Some(&p.path_b)).await?;
        Ok(Json(analysis::diff(&a.board, &b.board, clamp_limit(p.limit, 200, 5000))))
    }
}

fn kind_name(kind: PartKind) -> String {
    serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ObvServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")).with_title("OpenBoardView MCP"),
            )
            .with_instructions(INSTRUCTIONS)
    }
}
