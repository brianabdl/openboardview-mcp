//! Decryption keys for the encrypted formats (.fz, .cae, XZZPCB).
//!
//! OpenBoardView stores them in its `obv.conf`; this module reads them from
//! there using the same rules as `Config::DecodeKey` / `Config::SetXZZPCBKey`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const KEY_WORDS: usize = 44;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecryptionKeys {
    pub fz: Option<Vec<u32>>,
    pub cae: Option<Vec<u32>>,
    pub xzz: Option<u64>,
}

impl DecryptionKeys {
    /// Loads keys from `config` if given, otherwise from OpenBoardView's
    /// default config file. A missing file simply yields no keys.
    pub fn load(config: Option<&Path>) -> Self {
        let path = match config {
            Some(path) => Some(path.to_path_buf()),
            None => default_config_path(),
        };
        let Some(path) = path else {
            return Self::default();
        };
        match std::fs::read(&path) {
            Ok(bytes) => Self::from_config_text(&String::from_utf8_lossy(&bytes)),
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("cannot read {}: {e}", path.display());
                }
                Self::default()
            }
        }
    }

    pub fn from_config_text(text: &str) -> Self {
        Self {
            fz: config_value(text, "FZKey").and_then(decode_rc6_key),
            cae: config_value(text, "CAEKey").and_then(decode_rc6_key),
            xzz: config_value(text, "XZZPCBKey").and_then(decode_xzz_key),
        }
    }

    pub fn summary(&self) -> String {
        let flag = |set: bool| if set { "set" } else { "not set" };
        format!(
            "FZ key {}, CAE key {}, XZZPCB key {}",
            flag(self.fz.is_some()),
            flag(self.cae.is_some()),
            flag(self.xzz.is_some())
        )
    }
}

/// `$XDG_CONFIG_HOME/OpenBoardView/obv.conf` (or the platform equivalent).
pub fn default_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("OpenBoardView").join("obv.conf"))
}

/// Value of a `key = value` line, matched at the start of a line like
/// OpenBoardView's `Confparse::Parse`.
pub fn config_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let rest = line.strip_prefix(key)?;
        if rest.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            return None; // a longer key sharing this prefix
        }
        let value = rest.trim_start_matches(['=', ' ', '\t']).trim_end();
        (!value.is_empty()).then_some(value)
    })
}

/// FZ/CAE keys are 44 comma-separated 32-bit hex words (`0x12345678, ...`).
fn decode_rc6_key(text: &str) -> Option<Vec<u32>> {
    let words: Vec<u32> = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .filter_map(|w| {
            let hex = w.trim_start_matches("0x").trim_start_matches("0X");
            u32::from_str_radix(hex, 16).ok()
        })
        .collect();
    (words.len() == KEY_WORDS && words.iter().any(|&w| w != 0)).then_some(words)
}

/// XZZPCB key is a single 64-bit integer, usually hex with a `0x` prefix
/// (parsed like `std::stoul(text, nullptr, 0)`).
fn decode_xzz_key(text: &str) -> Option<u64> {
    let text = text.trim();
    let value = if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()?
    } else if text.len() > 1 && text.starts_with('0') {
        u64::from_str_radix(&text[1..], 8).ok()?
    } else {
        text.parse().ok()?
    };
    (value != 0).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_values_at_line_start_only() {
        let conf = "# FZKey = nope\nFZKeyOther = 1\nXZZPCBKey = 0xABCDEF\n";
        assert_eq!(config_value(conf, "FZKey"), None);
        assert_eq!(config_value(conf, "XZZPCBKey"), Some("0xABCDEF"));
    }

    #[test]
    fn decodes_full_rc6_key() {
        let words: Vec<String> = (1..=44).map(|i| format!("0x{i:08x}")).collect();
        let conf = format!("FZKey = {}\n", words.join(", "));
        let keys = DecryptionKeys::from_config_text(&conf);
        assert_eq!(keys.fz.as_deref().map(<[u32]>::len), Some(44));
        assert_eq!(keys.fz.unwrap()[43], 44);
        assert_eq!(keys.cae, None);
    }

    #[test]
    fn rejects_partial_or_empty_keys() {
        let keys = DecryptionKeys::from_config_text("FZKey = 0x1, 0x2\nCAEKey = \nXZZPCBKey = 0\n");
        assert_eq!(keys, DecryptionKeys::default());
    }

    #[test]
    fn decodes_xzz_key_bases() {
        assert_eq!(decode_xzz_key("0x10"), Some(16));
        assert_eq!(decode_xzz_key("16"), Some(16));
        assert_eq!(decode_xzz_key("020"), Some(16));
        assert_eq!(decode_xzz_key("zz"), None);
    }
}
