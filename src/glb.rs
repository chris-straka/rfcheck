// SPDX-License-Identifier: MIT
//! glTF 2.0 binary container parsing. Corrupt input is always a
//! diagnostic, never a panic: every read is bounds-checked.

use serde_json::Value;

pub const MAGIC_GLTF: &[u8; 4] = b"glTF";
pub const CHUNK_JSON: u32 = 0x4E4F534A;
pub const CHUNK_BIN: u32 = 0x004E4942;

pub struct Document<'a> {
    pub json: Value,
    pub bin: &'a [u8],
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let w: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(w))
}

/// Parse the container; on success return the JSON chunk.
/// Err is (diagnostic code, detail).
pub fn parse(bytes: &[u8]) -> Result<Document<'_>, (&'static str, String)> {
    if bytes.len() < 12 || &bytes[0..4] != MAGIC_GLTF {
        return Err(("D_GLB_MAGIC", "not a glTF binary container".to_string()));
    }
    let version = read_u32(bytes, 4).unwrap_or(0);
    if version != 2 {
        return Err((
            "D_GLB_VERSION",
            format!("container version is {version}, expected 2"),
        ));
    }
    let total = read_u32(bytes, 8).unwrap_or(0) as usize;
    if total > bytes.len() {
        return Err((
            "D_GLB_TRUNC",
            format!("declared length {total} exceeds file size {}", bytes.len()),
        ));
    }
    // Chunks must fit the declared length, not merely the file: a
    // header that understates it would otherwise silently drop BIN.
    let bytes = &bytes[..total];
    let at = 12usize;
    let chunk_len = match read_u32(bytes, at) {
        Some(v) => v as usize,
        None => {
            return Err((
                "D_GLB_JSON",
                format!("missing JSON chunk (declared length {total})"),
            ))
        }
    };
    let chunk_type = read_u32(bytes, at + 4).unwrap_or(0);
    if chunk_type != CHUNK_JSON {
        return Err((
            "D_GLB_JSON",
            format!("first chunk type is {chunk_type:#X}, expected JSON"),
        ));
    }
    let start = at + 8;
    let end = match start.checked_add(chunk_len) {
        Some(e) => e,
        None => {
            return Err(("D_GLB_TRUNC", "JSON chunk length overflows".to_string()));
        }
    };
    if end > bytes.len() {
        return Err((
            "D_GLB_TRUNC",
            format!("JSON chunk extends past declared length {total}"),
        ));
    }
    let json: Value = match serde_json::from_slice::<Value>(&bytes[start..end]) {
        Ok(json) => json,
        Err(e) => return Err(("D_GLB_JSON", format!("JSON chunk does not parse: {e}"))),
    };
    let mut bin: &[u8] = &[];
    if end < total {
        let blen = match read_u32(bytes, end) {
            Some(v) => v as usize,
            None => {
                return Err((
                    "D_GLB_TRUNC",
                    format!("BIN chunk header extends past declared length {total}"),
                ))
            }
        };
        let btype = read_u32(bytes, end + 4).unwrap_or(0);
        let bstart = end + 8;
        let bend = match bstart.checked_add(blen) {
            Some(e) => e,
            None => return Err(("D_GLB_TRUNC", "BIN chunk length overflows".to_string())),
        };
        if bend > bytes.len() {
            return Err((
                "D_GLB_TRUNC",
                format!("BIN chunk extends past declared length {total}"),
            ));
        }
        // Unknown second-chunk types are ignored (forward-compatible);
        // skinning reads then fail loudly as W_BAD_ACCESSOR, never silently.
        if btype == CHUNK_BIN {
            bin = &bytes[bstart..bend];
        }
    }
    Ok(Document { json, bin })
}
