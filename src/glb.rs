// SPDX-License-Identifier: MIT
//! glTF 2.0 binary container parsing. Corrupt input is always a
//! diagnostic, never a panic: every read is bounds-checked.

use serde_json::Value;

pub const MAGIC_GLTF: &[u8; 4] = b"glTF";
pub const CHUNK_JSON: u32 = 0x4E4F534A;

pub struct Document {
    pub json: Value,
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let w: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(w))
}

/// Parse the container; on success return the JSON chunk.
/// Err is (diagnostic code, detail).
pub fn parse(bytes: &[u8]) -> Result<Document, (&'static str, String)> {
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
    let at = 12usize;
    let chunk_len = match read_u32(bytes, at) {
        Some(v) => v as usize,
        None => return Err(("D_GLB_JSON", "missing JSON chunk".to_string())),
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
            "JSON chunk extends past end of file".to_string(),
        ));
    }
    match serde_json::from_slice::<Value>(&bytes[start..end]) {
        Ok(json) => Ok(Document { json }),
        Err(e) => Err(("D_GLB_JSON", format!("JSON chunk does not parse: {e}"))),
    }
}
