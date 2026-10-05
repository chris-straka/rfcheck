// SPDX-License-Identifier: MIT
//! glTF 2.0 binary container parsing (glbkit) mapped onto rfcheck's
//! diagnostic codes. Corrupt input is always a diagnostic, never a panic.

use glbkit::container::{self, ErrorKind};
use serde_json::Value;

pub struct Document<'a> {
    pub json: Value,
    pub bin: &'a [u8],
}

/// Parse the container; on success return the JSON chunk.
/// Err is (diagnostic code, detail). An unknown second chunk is ignored
/// (forward-compatible); skinning reads then fail loudly as
/// W_BAD_ACCESSOR, never silently.
pub fn parse(bytes: &[u8]) -> Result<Document<'_>, (&'static str, String)> {
    let c = container::parse(bytes).map_err(|e| {
        let code = match e.kind {
            ErrorKind::Magic => "D_GLB_MAGIC",
            ErrorKind::Version => "D_GLB_VERSION",
            ErrorKind::Json => "D_GLB_JSON",
            ErrorKind::Truncated | ErrorKind::TooLarge => "D_GLB_TRUNC",
        };
        (code, e.detail)
    })?;
    let json = serde_json::from_slice::<Value>(c.json)
        .map_err(|e| ("D_GLB_JSON", format!("JSON chunk does not parse: {e}")))?;
    Ok(Document { json, bin: c.bin })
}
