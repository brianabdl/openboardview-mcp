//! Bindings to the C wrapper in `native/obv_ffi.h`.
//!
//! The C++ parsers are trusted to the extent OpenBoardView trusts them, which
//! is not much: a malformed file can crash them. The server therefore never
//! calls [`parse_file`] itself; it runs in the parse worker process (see
//! [`super::worker`]).

use std::ffi::{CStr, CString, c_char, c_int};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

use super::keys::{DecryptionKeys, KEY_WORDS};
use super::raw::{RawBoard, RawNail, RawPart, RawPartType, RawPin, RawPoint, RawSide};

// libz-sys provides the zlib symbols the FZ parser links against.
use libz_sys as _;

#[repr(C)]
#[derive(Clone, Copy)]
struct ObvPoint {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ObvSegment {
    a: ObvPoint,
    b: ObvPoint,
}

#[repr(C)]
struct ObvPart {
    name: *const c_char,
    mfgcode: *const c_char,
    mounting_side: u8,
    part_type: u8,
    end_of_pins: u32,
    p1: ObvPoint,
    p2: ObvPoint,
}

#[repr(C)]
struct ObvPin {
    pos: ObvPoint,
    probe: i32,
    part: u32,
    side: u8,
    radius: f64,
    net: *const c_char,
    snum: *const c_char,
    name: *const c_char,
}

#[repr(C)]
struct ObvNail {
    probe: u32,
    pos: ObvPoint,
    side: u8,
    net: *const c_char,
}

#[repr(C)]
struct ObvKeys {
    fz_key: *const u32,
    cae_key: *const u32,
    xzz_key: u64,
    has_xzz_key: u8,
}

#[repr(C)]
struct ObvBoard {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn obv_board_parse(path: *const c_char, keys: *const ObvKeys) -> *mut ObvBoard;
    fn obv_board_free(board: *mut ObvBoard);
    fn obv_board_valid(board: *const ObvBoard) -> c_int;
    fn obv_board_error(board: *const ObvBoard) -> *const c_char;
    fn obv_board_format(board: *const ObvBoard) -> *const c_char;
    fn obv_board_outline_points(board: *const ObvBoard, count: *mut usize) -> *const ObvPoint;
    fn obv_board_outline_segments(board: *const ObvBoard, count: *mut usize) -> *const ObvSegment;
    fn obv_board_parts(board: *const ObvBoard, count: *mut usize) -> *const ObvPart;
    fn obv_board_pins(board: *const ObvBoard, count: *mut usize) -> *const ObvPin;
    fn obv_board_nails(board: *const ObvBoard, count: *mut usize) -> *const ObvNail;
}

/// Owns a parser handle and frees it on drop.
struct Handle(*mut ObvBoard);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the pointer came from obv_board_parse and is freed exactly once.
        unsafe { obv_board_free(self.0) }
    }
}

impl Handle {
    fn slice<T>(&self, get: unsafe extern "C" fn(*const ObvBoard, *mut usize) -> *const T) -> &[T] {
        let mut count = 0usize;
        // SAFETY: the handle is valid; the returned array lives as long as the handle.
        let ptr = unsafe { get(self.0, &mut count) };
        if ptr.is_null() || count == 0 {
            &[]
        } else {
            // SAFETY: the wrapper guarantees `count` initialized elements at `ptr`.
            unsafe { std::slice::from_raw_parts(ptr, count) }
        }
    }
}

/// Copies a C string owned by the handle. Invalid UTF-8 is replaced, since
/// board files frequently carry legacy 8-bit encodings.
fn string(ptr: *const c_char) -> String {
    opt_string(ptr).unwrap_or_default()
}

fn opt_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null strings from the wrapper are NUL-terminated and outlive this call.
    Some(unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
}

/// The FZ/CAE parsers append the key they tried to their error messages
/// ("FZ Key:\n 0x..."). Keys are secrets, so cut the message there.
fn redact_keys(message: &str) -> String {
    let cut = message.find("FZ Key:").into_iter().chain(message.find("CAE Key:")).min();
    let kept = cut.map_or(message, |i| &message[..i]);
    kept.trim().trim_end_matches(':').trim().to_string()
}

fn point(p: ObvPoint) -> RawPoint {
    RawPoint { x: p.x, y: p.y }
}

/// Parses a board file in-process. Not thread-safe (the parsers change the
/// process locale); see the module documentation for why this runs in a
/// separate process.
pub fn parse_file(path: &Path, keys: &DecryptionKeys) -> Result<RawBoard> {
    let path_str = path.to_str().ok_or_else(|| anyhow!("path is not valid UTF-8"))?;
    let c_path = CString::new(path_str).context("path contains a NUL byte")?;

    let check = |key: &Option<Vec<u32>>, name: &str| -> Result<()> {
        match key {
            Some(words) if words.len() != KEY_WORDS => bail!("{name} key must have {KEY_WORDS} words"),
            _ => Ok(()),
        }
    };
    check(&keys.fz, "FZ")?;
    check(&keys.cae, "CAE")?;

    let ffi_keys = ObvKeys {
        fz_key: keys.fz.as_ref().map_or(std::ptr::null(), |k| k.as_ptr()),
        cae_key: keys.cae.as_ref().map_or(std::ptr::null(), |k| k.as_ptr()),
        xzz_key: keys.xzz.unwrap_or(0),
        has_xzz_key: keys.xzz.is_some() as u8,
    };

    // SAFETY: both pointers are valid for the duration of the call.
    let raw = unsafe { obv_board_parse(c_path.as_ptr(), &ffi_keys) };
    if raw.is_null() {
        bail!("out of memory while parsing");
    }
    let handle = Handle(raw);

    // SAFETY: valid handle.
    if unsafe { obv_board_valid(handle.0) } == 0 {
        let error = string(unsafe { obv_board_error(handle.0) });
        bail!("{}", redact_keys(&error));
    }

    let format = string(unsafe { obv_board_format(handle.0) });

    let parts = handle
        .slice(obv_board_parts)
        .iter()
        .map(|p| RawPart {
            name: string(p.name),
            mfgcode: string(p.mfgcode),
            side: RawSide::from_ffi(p.mounting_side),
            part_type: if p.part_type == 1 { RawPartType::ThroughHole } else { RawPartType::Smd },
            end_of_pins: p.end_of_pins,
            p1: point(p.p1),
            p2: point(p.p2),
        })
        .collect();

    let pins = handle
        .slice(obv_board_pins)
        .iter()
        .map(|p| RawPin {
            pos: point(p.pos),
            probe: p.probe,
            part: p.part,
            side: RawSide::from_ffi(p.side),
            radius: p.radius,
            net: string(p.net),
            number: opt_string(p.snum),
            name: opt_string(p.name),
        })
        .collect();

    let nails = handle
        .slice(obv_board_nails)
        .iter()
        .map(|n| RawNail { probe: n.probe, pos: point(n.pos), side: RawSide::from_ffi(n.side), net: string(n.net) })
        .collect();

    Ok(RawBoard {
        format,
        outline_points: handle.slice(obv_board_outline_points).iter().copied().map(point).collect(),
        outline_segments: handle.slice(obv_board_outline_segments).iter().map(|s| (point(s.a), point(s.b))).collect(),
        parts,
        pins,
        nails,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_key_material() {
        let msg = "Invalid FZ key\nFZ Key:\n 0x12345678 0x9abcdef0\n";
        assert_eq!(redact_keys(msg), "Invalid FZ key");
        assert_eq!(redact_keys("Invalid CAE key\nCAE Key:\n 0x1"), "Invalid CAE key");
        assert_eq!(redact_keys("Unrecognized file format."), "Unrecognized file format.");
    }
}
