//! Board annotations, stored exactly where and how OpenBoardView stores them,
//! so notes written here show up in the GUI and vice versa.
//!
//! OpenBoardView keeps one SQLite database per board file: the board path
//! with its last `.` replaced by `_`, plus `.sqlite3` (`foo.brd` becomes
//! `foo_brd.sqlite3`). Deleting only hides a row (`visible = 0`), as in the GUI.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, params};
use schemars::JsonSchema;
use serde::Serialize;

/// Same schema as `Annotations::Init` in OpenBoardView.
const CREATE_TABLE: &str = "CREATE TABLE IF NOT EXISTS annotations(\
    id INTEGER PRIMARY KEY AUTOINCREMENT,\
    visible INTEGER,\
    pin TEXT,\
    part TEXT,\
    net TEXT,\
    posx INTEGER,\
    posy INTEGER,\
    side INTEGER,\
    note TEXT);";

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Annotation {
    pub id: i64,
    /// 0 = top, 1 = bottom (the side that was being viewed when it was added).
    pub side: i64,
    /// Position in board coordinates (mils).
    pub x: f64,
    pub y: f64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub net: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub part: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub pin: String,
    pub note: String,
}

#[derive(Debug, Clone)]
pub struct NewAnnotation {
    pub side: i64,
    pub x: f64,
    pub y: f64,
    pub net: String,
    pub part: String,
    pub pin: String,
    pub note: String,
}

/// Database path OpenBoardView uses for `board` (`Annotations::Open`).
pub fn database_path(board: &Path) -> PathBuf {
    let mut s = board.to_string_lossy().into_owned();
    if let Some(pos) = s.rfind('.') {
        s.replace_range(pos..pos + 1, "_");
    }
    s.push_str(".sqlite3");
    PathBuf::from(s)
}

fn open(board: &Path, create: bool) -> Result<Option<Connection>> {
    let path = database_path(board);
    if !create && !path.exists() {
        return Ok(None);
    }
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if create {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let conn = Connection::open_with_flags(&path, flags)
        .with_context(|| format!("cannot open annotation database {}", path.display()))?;
    // The GUI may hold the database open; wait briefly instead of failing.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(CREATE_TABLE)?;
    Ok(Some(conn))
}

pub fn list(board: &Path) -> Result<Vec<Annotation>> {
    let Some(conn) = open(board, false)? else {
        return Ok(Vec::new());
    };
    let mut stmt = conn
        .prepare("SELECT id, side, posx, posy, net, part, pin, note FROM annotations WHERE visible = 1 ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok(Annotation {
            id: row.get(0)?,
            side: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
            x: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            y: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            net: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            part: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            pin: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
            note: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().context("reading annotations")
}

pub fn add(board: &Path, a: &NewAnnotation) -> Result<i64> {
    let conn = open(board, true)?.expect("created");
    conn.execute(
        "INSERT INTO annotations (visible, side, posx, posy, net, part, pin, note) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![a.side, a.x, a.y, a.net, a.part, a.pin, a.note],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update(board: &Path, id: i64, note: &str) -> Result<()> {
    let Some(conn) = open(board, false)? else {
        bail!("this board has no annotations");
    };
    let changed = conn.execute("UPDATE annotations SET note = ?1 WHERE id = ?2 AND visible = 1", params![note, id])?;
    if changed == 0 {
        bail!("no annotation with id {id}");
    }
    Ok(())
}

pub fn delete(board: &Path, id: i64) -> Result<()> {
    let Some(conn) = open(board, false)? else {
        bail!("this board has no annotations");
    };
    let changed = conn.execute("UPDATE annotations SET visible = 0 WHERE id = ?1 AND visible = 1", params![id])?;
    if changed == 0 {
        bail!("no annotation with id {id}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_path_matches_openboardview() {
        assert_eq!(database_path(Path::new("/x/board.brd")), PathBuf::from("/x/board_brd.sqlite3"));
        assert_eq!(database_path(Path::new("/x/a.b.fz")), PathBuf::from("/x/a.b_fz.sqlite3"));
    }

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("obv-mcp-ann-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let board = dir.join("t.brd");
        assert!(list(&board).unwrap().is_empty());

        let id = add(
            &board,
            &NewAnnotation {
                side: 0,
                x: 10.0,
                y: 20.0,
                net: "PP3V3".into(),
                part: "C1".into(),
                pin: "1".into(),
                note: "shorted".into(),
            },
        )
        .unwrap();
        update(&board, id, "shorted to GND").unwrap();
        let all = list(&board).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].note, "shorted to GND");

        delete(&board, id).unwrap();
        assert!(list(&board).unwrap().is_empty());
        assert!(delete(&board, id).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
