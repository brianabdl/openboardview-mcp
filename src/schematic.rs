//! Schematic PDF linked to a board.
//!
//! OpenBoardView looks for the PDF in the board's `.conf` file (`PDFFilePath`,
//! relative to the config's directory) and otherwise next to the board with a
//! `.pdf` extension. Text is extracted with poppler's `pdftotext`, page by
//! page, and cached until the PDF changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{Context, Result, anyhow, bail};
use schemars::JsonSchema;
use serde::Serialize;

use crate::parse::keys::config_value;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SchematicLocation {
    /// Resolved PDF path (whether or not it exists).
    pub pdf: Option<String>,
    pub exists: bool,
    /// Where the path came from: `board_conf` or `same_name`.
    pub source: String,
    pub board_conf: String,
}

/// Where OpenBoardView would look for this board's schematic.
pub fn locate(board: &Path) -> SchematicLocation {
    let conf = board.with_extension("conf");
    if let Ok(text) = std::fs::read_to_string(&conf)
        && let Some(rel) = config_value(&text, "PDFFilePath")
    {
        let dir = conf.parent().unwrap_or(Path::new("."));
        let pdf = dir.join(rel);
        return SchematicLocation {
            exists: pdf.is_file(),
            pdf: Some(pdf.display().to_string()),
            source: "board_conf".into(),
            board_conf: conf.display().to_string(),
        };
    }
    let pdf = board.with_extension("pdf");
    SchematicLocation {
        exists: pdf.is_file(),
        pdf: Some(pdf.display().to_string()),
        source: "same_name".into(),
        board_conf: conf.display().to_string(),
    }
}

/// Points the board at a schematic by writing `PDFFilePath` into its `.conf`,
/// the same setting OpenBoardView's Board Settings dialog writes.
pub fn link(board: &Path, pdf: &Path) -> Result<PathBuf> {
    if !pdf.is_file() {
        bail!("{} is not a file", pdf.display());
    }
    let conf = board.with_extension("conf");
    let dir = conf.parent().unwrap_or(Path::new("."));
    let value = pdf.strip_prefix(dir).map(Path::to_path_buf).unwrap_or_else(|_| pdf.to_path_buf());
    let line = format!("PDFFilePath = {}", value.display());

    let existing = std::fs::read_to_string(&conf).unwrap_or_default();
    let mut replaced = false;
    let mut lines: Vec<String> = existing
        .lines()
        .map(|l| {
            if config_value(l, "PDFFilePath").is_some() || l.trim() == "PDFFilePath =" {
                replaced = true;
                line.clone()
            } else {
                l.to_string()
            }
        })
        .collect();
    if !replaced {
        lines.push(line);
    }
    std::fs::write(&conf, lines.join("\n") + "\n").with_context(|| format!("cannot write {}", conf.display()))?;
    Ok(conf)
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SchematicHit {
    /// 1-based page number.
    pub page: usize,
    /// The matching line, trimmed.
    pub line: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PageSummary {
    pub page: usize,
    pub matches: usize,
}

/// Extracted page texts of one PDF, with the modification time they match.
type CachedPages = (Option<SystemTime>, Arc<Vec<String>>);

#[derive(Default)]
pub struct SchematicCache {
    pages: Mutex<HashMap<PathBuf, CachedPages>>,
}

impl SchematicCache {
    /// Page texts of `pdf`, extracting them on first use.
    pub async fn pages(&self, pdf: &Path) -> Result<Arc<Vec<String>>> {
        let modified = std::fs::metadata(pdf).and_then(|m| m.modified()).ok();
        if let Some((m, pages)) = self.pages.lock().unwrap().get(pdf)
            && *m == modified
        {
            return Ok(pages.clone());
        }
        let pages = Arc::new(extract_pages(pdf).await?);
        self.pages.lock().unwrap().insert(pdf.to_path_buf(), (modified, pages.clone()));
        Ok(pages)
    }
}

async fn extract_pages(pdf: &Path) -> Result<Vec<String>> {
    let output = tokio::process::Command::new("pdftotext")
        .arg("-layout")
        .arg("-enc")
        .arg("UTF-8")
        .arg(pdf)
        .arg("-")
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow!("`pdftotext` was not found; install poppler-utils (or poppler) to search schematics")
            } else {
                anyhow!("cannot run pdftotext: {e}")
            }
        })?;
    if !output.status.success() {
        bail!("pdftotext failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut pages: Vec<String> = text.split('\x0c').map(str::to_string).collect();
    if pages.last().is_some_and(|p| p.trim().is_empty()) {
        pages.pop();
    }
    Ok(pages)
}

/// Case-insensitive search. With `whole_word`, a match must not be part of a
/// longer identifier, so `C12` does not match `C123`.
pub fn search(pages: &[String], query: &str, whole_word: bool, limit: usize) -> (Vec<SchematicHit>, Vec<PageSummary>) {
    let q = query.trim().to_lowercase();
    let mut hits = Vec::new();
    let mut summary = Vec::new();
    if q.is_empty() {
        return (hits, summary);
    }
    for (i, page) in pages.iter().enumerate() {
        let mut count = 0;
        for line in page.lines() {
            let n = count_matches(&line.to_lowercase(), &q, whole_word);
            if n > 0 {
                count += n;
                if hits.len() < limit {
                    hits.push(SchematicHit { page: i + 1, line: collapse_spaces(line.trim()) });
                }
            }
        }
        if count > 0 {
            summary.push(PageSummary { page: i + 1, matches: count });
        }
    }
    (hits, summary)
}

fn count_matches(haystack: &str, needle: &str, whole_word: bool) -> usize {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    haystack
        .match_indices(needle)
        .filter(|(start, m)| {
            if !whole_word {
                return true;
            }
            let before = haystack[..*start].chars().next_back();
            let after = haystack[start + m.len()..].chars().next();
            !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
        })
        .count()
}

/// Layout mode pads columns with runs of spaces; shorten them for display.
fn collapse_spaces(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut spaces = 0;
    for c in line.chars() {
        if c == ' ' {
            spaces += 1;
            if spaces <= 3 {
                out.push(c);
            }
        } else {
            spaces = 0;
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_word_search() {
        let pages = vec!["C12 10uF\nC123 1uF".to_string(), "net C12_X".to_string()];
        let (hits, summary) = search(&pages, "c12", true, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(summary.len(), 1);
        let (hits, _) = search(&pages, "c12", false, 10);
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn link_writes_relative_path() {
        let dir = std::env::temp_dir().join(format!("obv-mcp-sch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let board = dir.join("b.brd");
        let pdf = dir.join("schem.pdf");
        std::fs::write(&pdf, b"%PDF").unwrap();
        std::fs::write(board.with_extension("conf"), "BackgroundImage = x.png\nPDFFilePath = old.pdf\n").unwrap();
        link(&board, &pdf).unwrap();
        let conf = std::fs::read_to_string(board.with_extension("conf")).unwrap();
        assert!(conf.contains("PDFFilePath = schem.pdf"));
        assert!(conf.contains("BackgroundImage = x.png"));
        let loc = locate(&board);
        assert!(loc.exists);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
