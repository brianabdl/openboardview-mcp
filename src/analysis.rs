//! Board queries used by the tools: name matching, search, and the
//! connectivity and proximity analyses that support board-level repair.

use std::collections::{HashMap, HashSet, VecDeque};

use schemars::JsonSchema;
use serde::Serialize;

use crate::board::{Board, NetKind, PartKind, Point, Rect, UNCONNECTED_NET};

// ---------------------------------------------------------------------------
// Name matching
// ---------------------------------------------------------------------------

/// Case-insensitive name filter: a glob when the pattern contains `*` or `?`,
/// otherwise a substring match.
pub struct NameFilter {
    pattern: String,
    glob: bool,
}

impl NameFilter {
    pub fn new(pattern: &str) -> Self {
        let pattern = pattern.trim().to_ascii_uppercase();
        let glob = pattern.contains(['*', '?']);
        Self { pattern, glob }
    }

    pub fn matches(&self, name: &str) -> bool {
        let name = name.to_ascii_uppercase();
        if self.glob { glob_match(self.pattern.as_bytes(), name.as_bytes()) } else { name.contains(&self.pattern) }
    }
}

/// Iterative wildcard matcher (`*` = any run, `?` = any single byte).
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HitKind {
    Part,
    Net,
    /// Match on a part's part number / value; `name` is the part.
    PartNumber,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SearchHit {
    pub kind: HitKind,
    pub name: String,
    /// What matched, when different from `name` (the part number).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched: Option<String>,
    /// 0..1, higher is better.
    pub score: f64,
}

/// Uppercase alphanumerics only, so `+3V3_S5`, `3V3 S5` and `3v3s5` compare equal.
fn normalize(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect()
}

/// Ranks `candidate` against an uppercase `query`; `None` means no match.
fn score(query: &str, candidate: &str) -> Option<f64> {
    let c = candidate.to_ascii_uppercase();
    let ratio = |a: &str, b: &str| a.len() as f64 / b.len().max(1) as f64;
    if c == query {
        return Some(1.0);
    }
    if c.starts_with(query) {
        return Some(0.9 + 0.05 * ratio(query, &c));
    }
    if c.contains(query) {
        return Some(0.75 + 0.05 * ratio(query, &c));
    }
    let (nq, nc) = (normalize(query), normalize(&c));
    if nq.is_empty() || nc.is_empty() {
        return None;
    }
    if nc == nq {
        Some(0.95)
    } else if nc.starts_with(&nq) {
        Some(0.85 + 0.05 * ratio(&nq, &nc))
    } else if nc.contains(&nq) {
        Some(0.7 + 0.05 * ratio(&nq, &nc))
    } else {
        let jw = strsim::jaro_winkler(&nq, &nc);
        (jw >= 0.86).then_some(jw * 0.7)
    }
}

/// Searches part names, net names and part numbers. Globs (`*`, `?`) match
/// exactly; plain text is ranked with prefix, substring and fuzzy matching so
/// typos still find something.
pub fn search(board: &Board, query: &str, limit: usize) -> Vec<SearchHit> {
    let q = query.trim().to_ascii_uppercase();
    if q.is_empty() {
        return Vec::new();
    }
    let glob = NameFilter::new(&q);
    let use_glob = q.contains(['*', '?']);
    let rank = |candidate: &str| -> Option<f64> {
        if use_glob { glob.matches(candidate).then_some(1.0) } else { score(&q, candidate) }
    };

    let mut hits = Vec::new();
    for part in &board.parts {
        if let Some(s) = rank(&part.name) {
            hits.push(SearchHit { kind: HitKind::Part, name: part.name.clone(), matched: None, score: s });
        } else if !part.mfgcode.is_empty()
            && let Some(s) = rank(&part.mfgcode)
        {
            hits.push(SearchHit {
                kind: HitKind::PartNumber,
                name: part.name.clone(),
                matched: Some(part.mfgcode.clone()),
                score: s * 0.95,
            });
        }
    }
    for net in &board.nets {
        if let Some(s) = rank(&net.name) {
            hits.push(SearchHit { kind: HitKind::Net, name: net.name.clone(), matched: None, score: s });
        }
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.name.cmp(&b.name)));
    hits.truncate(limit);
    for hit in &mut hits {
        hit.score = (hit.score * 1000.0).round() / 1000.0;
    }
    hits
}

/// Closest names for a "not found" message.
pub fn suggest<'a>(names: impl Iterator<Item = &'a str>, query: &str, count: usize) -> Vec<String> {
    let q = query.trim().to_ascii_uppercase();
    let mut scored: Vec<(f64, &str)> = names
        .filter_map(|n| {
            let s = score(&q, n).unwrap_or_else(|| {
                let upper = n.to_ascii_uppercase();
                strsim::jaro_winkler(&q, &upper).max(strsim::normalized_levenshtein(&q, &upper)) * 0.6
            });
            (s >= 0.3).then_some((s, n))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.into_iter().take(count).map(|(_, n)| n.to_string()).collect()
}

// ---------------------------------------------------------------------------
// Connectivity
// ---------------------------------------------------------------------------

/// A part with pins on two given nets.
#[derive(Debug, Clone)]
pub struct Bridge {
    pub part: usize,
    pub pins_a: Vec<usize>,
    pub pins_b: Vec<usize>,
}

/// Parts with at least one pin on a net in `a` and one on a net in `b`.
pub fn parts_between(board: &Board, a: &[usize], b: &[usize]) -> Vec<Bridge> {
    let mut out = Vec::new();
    for (idx, part) in board.parts.iter().enumerate() {
        let pins_a: Vec<usize> = part.pins.iter().copied().filter(|&p| a.contains(&board.pins[p].net)).collect();
        if pins_a.is_empty() {
            continue;
        }
        let pins_b: Vec<usize> =
            part.pins.iter().copied().filter(|&p| b.contains(&board.pins[p].net) && !pins_a.contains(&p)).collect();
        if !pins_b.is_empty() {
            out.push(Bridge { part: idx, pins_a, pins_b });
        }
    }
    out
}

pub fn ground_nets(board: &Board) -> Vec<usize> {
    (0..board.nets.len()).filter(|&n| board.is_ground(n)).collect()
}

/// One step of a net-to-net walk across a two-net part.
#[derive(Debug, Clone)]
pub struct NeighborStep {
    pub net: usize,
    pub depth: usize,
    pub via_part: usize,
    pub from_net: usize,
}

/// Breadth-first walk from `start` across parts that connect exactly two
/// nets and whose kind is in `through`. Ground and unconnected nets are never
/// crossed. Each reached net is reported once, at its shortest depth.
pub fn net_neighbors(board: &Board, start: usize, max_depth: usize, through: &[PartKind]) -> Vec<NeighborStep> {
    // Precompute two-net parts per net.
    let mut by_net: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();
    for (idx, part) in board.parts.iter().enumerate() {
        if !through.contains(&part.kind) {
            continue;
        }
        let nets: Vec<usize> =
            board.part_nets(idx).into_iter().filter(|&n| board.nets[n].kind != NetKind::Unconnected).collect();
        if let [a, b] = nets[..] {
            by_net.entry(a).or_default().push((idx, b));
            by_net.entry(b).or_default().push((idx, a));
        }
    }

    let mut seen: HashSet<usize> = HashSet::from([start]);
    let mut queue = VecDeque::from([(start, 0usize)]);
    let mut out = Vec::new();
    while let Some((net, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        for &(part, other) in by_net.get(&net).map(Vec::as_slice).unwrap_or(&[]) {
            if board.is_ground(other) || !seen.insert(other) {
                continue;
            }
            out.push(NeighborStep { net: other, depth: depth + 1, via_part: part, from_net: net });
            queue.push_back((other, depth + 1));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Proximity
// ---------------------------------------------------------------------------

/// Distance from `p` to the nearest point of `r` (0 inside).
pub fn distance_to_rect(p: Point, r: &Rect) -> f64 {
    if r.is_empty() {
        return f64::INFINITY;
    }
    let dx = (r.min.x - p.x).max(0.0).max(p.x - r.max.x);
    let dy = (r.min.y - p.y).max(0.0).max(p.y - r.max.y);
    (dx * dx + dy * dy).sqrt()
}

/// Parts whose body comes within `radius` mils of `center`, closest first.
pub fn nearby_parts(
    board: &Board,
    center: Point,
    radius: f64,
    side: crate::board::Side,
    exclude: Option<usize>,
) -> Vec<(usize, f64)> {
    let mut found: Vec<(usize, f64)> = board
        .parts
        .iter()
        .enumerate()
        .filter(|(i, p)| Some(*i) != exclude && p.side.visible_from(side))
        .map(|(i, p)| (i, distance_to_rect(center, &p.bbox)))
        .filter(|(_, d)| *d <= radius)
        .collect();
    found.sort_by(|a, b| a.1.total_cmp(&b.1));
    found
}

// ---------------------------------------------------------------------------
// Power rails
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RailStats {
    pub net: usize,
    pub caps_to_ground: Vec<usize>,
    pub inductors: Vec<usize>,
    pub ics: Vec<usize>,
    pub transistors: Vec<usize>,
}

/// Statistics for every net classified as power, largest first.
pub fn power_rails(board: &Board) -> Vec<RailStats> {
    let grounds = ground_nets(board);
    let mut rails: Vec<RailStats> = board
        .nets
        .iter()
        .enumerate()
        .filter(|(_, n)| n.kind == NetKind::Power && n.name != UNCONNECTED_NET)
        .map(|(net, _)| {
            let mut stats =
                RailStats { net, caps_to_ground: vec![], inductors: vec![], ics: vec![], transistors: vec![] };
            for part in board.net_parts(net) {
                let p = &board.parts[part];
                match p.kind {
                    PartKind::Capacitor => {
                        if board.part_nets(part).iter().any(|n| grounds.contains(n)) {
                            stats.caps_to_ground.push(part);
                        }
                    }
                    PartKind::Inductor => stats.inductors.push(part),
                    PartKind::Ic => stats.ics.push(part),
                    PartKind::Transistor => stats.transistors.push(part),
                    _ => {}
                }
            }
            stats
        })
        .collect();
    rails.sort_by(|a, b| {
        board.nets[b.net].pins.len().cmp(&board.nets[a.net].pins.len()).then_with(|| a.net.cmp(&b.net))
    });
    rails
}

// ---------------------------------------------------------------------------
// Board comparison
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PinNetChange {
    pub part: String,
    pub pin: String,
    pub net_a: String,
    pub net_b: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PartNumberChange {
    pub part: String,
    pub a: String,
    pub b: String,
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct BoardDiff {
    pub parts_only_in_a: Vec<String>,
    pub parts_only_in_b: Vec<String>,
    pub nets_only_in_a: Vec<String>,
    pub nets_only_in_b: Vec<String>,
    pub part_number_changes: Vec<PartNumberChange>,
    pub pin_net_changes: Vec<PinNetChange>,
    /// Total counts before truncation to `limit`.
    pub totals: DiffTotals,
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct DiffTotals {
    pub parts_only_in_a: usize,
    pub parts_only_in_b: usize,
    pub nets_only_in_a: usize,
    pub nets_only_in_b: usize,
    pub part_number_changes: usize,
    pub pin_net_changes: usize,
}

/// Compares two boards by part and net names (e.g. two revisions).
pub fn diff(a: &Board, b: &Board, limit: usize) -> BoardDiff {
    let mut d = BoardDiff::default();

    for part in &a.parts {
        match b.part(&part.name) {
            None => d.parts_only_in_a.push(part.name.clone()),
            Some(bi) => {
                let other = &b.parts[bi];
                if !part.mfgcode.eq_ignore_ascii_case(&other.mfgcode) {
                    d.part_number_changes.push(PartNumberChange {
                        part: part.name.clone(),
                        a: part.mfgcode.clone(),
                        b: other.mfgcode.clone(),
                    });
                }
                for &pin in &part.pins {
                    let pa = &a.pins[pin];
                    if let Some(pb) = b.part_pin(bi, &pa.number) {
                        let (na, nb) = (&a.nets[pa.net].name, &b.nets[b.pins[pb].net].name);
                        if !na.eq_ignore_ascii_case(nb) {
                            d.pin_net_changes.push(PinNetChange {
                                part: part.name.clone(),
                                pin: pa.number.clone(),
                                net_a: na.clone(),
                                net_b: nb.clone(),
                            });
                        }
                    }
                }
            }
        }
    }
    d.parts_only_in_b = b.parts.iter().filter(|p| a.part(&p.name).is_none()).map(|p| p.name.clone()).collect();
    d.nets_only_in_a = a.nets.iter().filter(|n| b.net(&n.name).is_none()).map(|n| n.name.clone()).collect();
    d.nets_only_in_b = b.nets.iter().filter(|n| a.net(&n.name).is_none()).map(|n| n.name.clone()).collect();

    d.totals = DiffTotals {
        parts_only_in_a: d.parts_only_in_a.len(),
        parts_only_in_b: d.parts_only_in_b.len(),
        nets_only_in_a: d.nets_only_in_a.len(),
        nets_only_in_b: d.nets_only_in_b.len(),
        part_number_changes: d.part_number_changes.len(),
        pin_net_changes: d.pin_net_changes.len(),
    };
    d.parts_only_in_a.truncate(limit);
    d.parts_only_in_b.truncate(limit);
    d.nets_only_in_a.truncate(limit);
    d.nets_only_in_b.truncate(limit);
    d.part_number_changes.truncate(limit);
    d.pin_net_changes.truncate(limit);
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match(b"C*", b"C123"));
        assert!(glob_match(b"*3V3*", b"PP3V3_S5"));
        assert!(glob_match(b"U?", b"U7"));
        assert!(!glob_match(b"U?", b"U70"));
        assert!(glob_match(b"*", b""));
        assert!(!glob_match(b"R*", b"C1"));
    }

    #[test]
    fn filter_modes() {
        assert!(NameFilter::new("3v3").matches("PP3V3_S5"));
        assert!(NameFilter::new("pp*s5").matches("PP3V3_S5"));
        assert!(!NameFilter::new("pp*s0").matches("PP3V3_S5"));
    }

    #[test]
    fn fuzzy_scores() {
        assert_eq!(score("PP3V3", "pp3v3"), Some(1.0));
        assert!(score("3V3S5", "PP3V3_S5").is_some());
        assert!(score("3VALS", "+3VA").is_some());
        assert!(score("XYZ", "PP3V3").is_none());
        assert!(score("U70", "U7000").unwrap() > score("U70", "XU70").unwrap());
    }

    #[test]
    fn suggestions_for_short_names() {
        let names = ["C1", "C2", "U1", "PP3V3"];
        let s = suggest(names.iter().copied(), "C3", 3);
        assert!(s.contains(&"C1".to_string()) && s.contains(&"C2".to_string()), "{s:?}");
    }

    #[test]
    fn rect_distance() {
        let r = Rect { min: Point::new(0.0, 0.0), max: Point::new(10.0, 10.0) };
        assert_eq!(distance_to_rect(Point::new(5.0, 5.0), &r), 0.0);
        assert_eq!(distance_to_rect(Point::new(13.0, 14.0), &r), 5.0);
    }
}
