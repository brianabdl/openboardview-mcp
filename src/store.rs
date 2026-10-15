//! Cache of parsed boards.
//!
//! Boards are keyed by canonical path and reparsed when the file's size or
//! modification time changes. The most recently used board is remembered so
//! tools can omit the path after a board has been opened.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use tokio::sync::Mutex;

use crate::board::Board;
use crate::parse::keys::DecryptionKeys;
use crate::parse::worker::parse_in_worker;

#[derive(Debug, Clone)]
pub struct StoreConfig {
    /// OpenBoardView config file holding decryption keys; `None` = default location.
    pub obv_config: Option<PathBuf>,
    pub parse_timeout: Duration,
    pub max_boards: usize,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self { obv_config: None, parse_timeout: Duration::from_secs(120), max_boards: 8 }
    }
}

/// A loaded board plus metadata about how it was loaded.
#[derive(Debug)]
pub struct LoadedBoard {
    pub board: Board,
    pub warnings: Vec<String>,
    pub parse_time: Duration,
}

struct Entry {
    modified: Option<SystemTime>,
    len: u64,
    loaded: Arc<LoadedBoard>,
    last_used: Instant,
}

#[derive(Default)]
struct Inner {
    boards: HashMap<PathBuf, Entry>,
    current: Option<PathBuf>,
}

pub struct BoardStore {
    config: StoreConfig,
    inner: Mutex<Inner>,
}

impl BoardStore {
    pub fn new(config: StoreConfig) -> Self {
        Self { config, inner: Mutex::new(Inner::default()) }
    }

    /// Returns the board at `path`, or the current board when `path` is `None`.
    pub async fn get(&self, path: Option<&str>) -> Result<Arc<LoadedBoard>> {
        let path = match path.map(str::trim).filter(|p| !p.is_empty()) {
            Some(p) => resolve_path(p)?,
            None => self
                .inner
                .lock()
                .await
                .current
                .clone()
                .ok_or_else(|| anyhow!("no board is open yet: pass `path`, or call open_board first"))?,
        };
        self.load(&path).await
    }

    pub async fn current_path(&self) -> Option<PathBuf> {
        self.inner.lock().await.current.clone()
    }

    async fn load(&self, path: &Path) -> Result<Arc<LoadedBoard>> {
        let meta = std::fs::metadata(path).with_context(|| format!("cannot access {}", path.display()))?;
        if !meta.is_file() {
            bail!("{} is not a file", path.display());
        }
        let modified = meta.modified().ok();
        let len = meta.len();

        // Holding the lock while parsing keeps concurrent requests for the
        // same file from parsing it twice; parses are short and infrequent.
        let mut inner = self.inner.lock().await;
        if let Some(entry) = inner.boards.get_mut(path)
            && entry.modified == modified
            && entry.len == len
        {
            entry.last_used = Instant::now();
            let loaded = entry.loaded.clone();
            inner.current = Some(path.to_path_buf());
            return Ok(loaded);
        }

        let keys = DecryptionKeys::load(self.config.obv_config.as_deref());
        let started = Instant::now();
        let outcome =
            parse_in_worker(path, &keys, self.config.parse_timeout).await.map_err(|e| enrich_error(e, path, &keys))?;
        let parse_time = started.elapsed();
        tracing::info!("parsed {} in {:?}", path.display(), parse_time);

        let loaded = Arc::new(LoadedBoard {
            board: Board::from_raw(path.to_path_buf(), outcome.board),
            warnings: outcome.warnings,
            parse_time,
        });

        if inner.boards.len() >= self.config.max_boards && !inner.boards.contains_key(path) {
            let oldest = inner.boards.iter().min_by_key(|(_, e)| e.last_used).map(|(p, _)| p.clone());
            if let Some(oldest) = oldest {
                inner.boards.remove(&oldest);
            }
        }
        inner
            .boards
            .insert(path.to_path_buf(), Entry { modified, len, loaded: loaded.clone(), last_used: Instant::now() });
        inner.current = Some(path.to_path_buf());
        Ok(loaded)
    }
}

/// Adds a hint about decryption keys when an encrypted format fails.
fn enrich_error(error: anyhow::Error, path: &Path, keys: &DecryptionKeys) -> anyhow::Error {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let missing = match ext.as_str() {
        "fz" if keys.fz.is_none() => Some("FZKey"),
        "cae" if keys.cae.is_none() => Some("CAEKey"),
        "pcb" if keys.xzz.is_none() => Some("XZZPCBKey"),
        _ => None,
    };
    match missing {
        Some(key) => error.context(format!(
            "this format is encrypted and no {key} is configured; set it in OpenBoardView \
             (Preferences > Program) or in obv.conf"
        )),
        None => error,
    }
}

/// Expands `~` and makes the path absolute and canonical.
pub fn resolve_path(input: &str) -> Result<PathBuf> {
    let expanded = if let Some(rest) = input.strip_prefix("~/") {
        dirs::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?.join(rest)
    } else {
        PathBuf::from(input)
    };
    std::fs::canonicalize(&expanded).with_context(|| format!("file not found: {}", expanded.display()))
}
