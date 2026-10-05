// SPDX-License-Identifier: MIT
//! glTF 2.0 binary container. Every read is bounds-checked.

use std::fmt;

pub const MAGIC: [u8; 4] = *b"glTF";
pub const CHUNK_JSON: u32 = 0x4E4F_534A;
pub const CHUNK_BIN: u32 = 0x004E_4942;

/// The two chunks of a GLB, borrowed from the input bytes. `bin` is empty
/// when the file has no BIN chunk.
#[derive(Debug, Clone, Copy)]
pub struct Container<'a> {
    pub json: &'a [u8],
    pub bin: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Too short for a header, or the magic is not `glTF`.
    Magic,
    /// Container version other than 2.
    Version,
    /// A declared length runs past the data.
    Truncated,
    /// The first chunk is missing or is not JSON.
    Json,
    /// [`write`] input that does not fit the u32 length fields.
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub detail: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for Error {}

fn fail<T>(kind: ErrorKind, detail: impl Into<String>) -> Result<T, Error> {
    Err(Error {
        kind,
        detail: detail.into(),
    })
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let w: [u8; 4] = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(w))
}

/// Split a GLB into its JSON and BIN chunks.
///
/// The JSON chunk must come first. Chunks must fit the header's declared
/// length (bytes after it are ignored). The first BIN chunk is kept;
/// unknown chunk types are skipped (forward-compatible).
pub fn parse(bytes: &[u8]) -> Result<Container<'_>, Error> {
    if bytes.len() < 12 || bytes[0..4] != MAGIC {
        return fail(ErrorKind::Magic, "not a glTF binary container");
    }
    let version = read_u32(bytes, 4).unwrap_or(0);
    if version != 2 {
        return fail(
            ErrorKind::Version,
            format!("container version is {version}, expected 2"),
        );
    }
    let total = read_u32(bytes, 8).unwrap_or(0) as usize;
    if total > bytes.len() {
        return fail(
            ErrorKind::Truncated,
            format!("declared length {total} exceeds file size {}", bytes.len()),
        );
    }
    // Chunks must fit the declared length, not merely the file: a header
    // that understates it would otherwise silently drop BIN.
    let bytes = &bytes[..total];
    let Some(json_len) = read_u32(bytes, 12) else {
        return fail(
            ErrorKind::Json,
            format!("missing JSON chunk (declared length {total})"),
        );
    };
    let json_type = read_u32(bytes, 16).unwrap_or(0);
    if json_type != CHUNK_JSON {
        return fail(
            ErrorKind::Json,
            format!("first chunk type is {json_type:#X}, expected JSON"),
        );
    }
    let json = chunk(bytes, 20, json_len, "JSON", total)?;
    let mut bin: &[u8] = &[];
    let mut at = 20 + json.len();
    while at < total {
        let (Some(len), Some(kind)) = (read_u32(bytes, at), read_u32(bytes, at + 4)) else {
            return fail(
                ErrorKind::Truncated,
                format!("chunk header at byte {at} extends past declared length {total}"),
            );
        };
        let name = if kind == CHUNK_BIN { "BIN" } else { "unknown" };
        let data = chunk(bytes, at + 8, len, name, total)?;
        if kind == CHUNK_BIN && bin.is_empty() {
            bin = data;
        }
        at += 8 + data.len();
    }
    Ok(Container { json, bin })
}

fn chunk<'a>(
    bytes: &'a [u8],
    start: usize,
    len: u32,
    name: &str,
    total: usize,
) -> Result<&'a [u8], Error> {
    let Some(end) = start.checked_add(len as usize) else {
        return fail(
            ErrorKind::Truncated,
            format!("{name} chunk length overflows"),
        );
    };
    match bytes.get(start..end) {
        Some(data) => Ok(data),
        None => fail(
            ErrorKind::Truncated,
            format!("{name} chunk extends past declared length {total}"),
        ),
    }
}

/// Pack a JSON document and BIN payload as a GLB.
///
/// Pads JSON with spaces and BIN with zeros to 4 bytes (the glTF spec, and
/// what Blender and Bevy's loader expect). An empty `bin` writes no BIN
/// chunk. The caller keeps `buffers[0].byteLength` equal to `bin.len()`
/// (padding bytes may follow it, per spec).
pub fn write(json: &[u8], bin: &[u8]) -> Result<Vec<u8>, Error> {
    let pad = |n: usize| (4 - n % 4) % 4;
    let json_len = json.len() + pad(json.len());
    let bin_len = bin.len() + pad(bin.len());
    let total = 12 + 8 + json_len + if bin.is_empty() { 0 } else { 8 + bin_len };
    let Ok(total32) = u32::try_from(total) else {
        return fail(
            ErrorKind::TooLarge,
            format!("GLB would be {total} bytes, over the 4 GiB container limit"),
        );
    };
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&total32.to_le_bytes());
    out.extend_from_slice(&(json_len as u32).to_le_bytes());
    out.extend_from_slice(&CHUNK_JSON.to_le_bytes());
    out.extend_from_slice(json);
    out.resize(out.len() + pad(json.len()), b' ');
    if !bin.is_empty() {
        out.extend_from_slice(&(bin_len as u32).to_le_bytes());
        out.extend_from_slice(&CHUNK_BIN.to_le_bytes());
        out.extend_from_slice(bin);
        out.resize(out.len() + pad(bin.len()), 0);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(bytes: &[u8]) -> ErrorKind {
        parse(bytes).unwrap_err().kind
    }

    #[test]
    fn round_trip_pads_and_reads_back() {
        let glb = write(br#"{"asset":{"version":"2.0"}}"#, &[1, 2, 3, 4, 5]).unwrap();
        assert_eq!(glb.len() % 4, 0);
        let c = parse(&glb).unwrap();
        assert_eq!(c.json, br#"{"asset":{"version":"2.0"}} "#);
        assert_eq!(c.bin, &[1, 2, 3, 4, 5, 0, 0, 0]);
    }

    #[test]
    fn json_only_has_no_bin_chunk() {
        let glb = write(b"{}", &[]).unwrap();
        assert_eq!(glb.len(), 12 + 8 + 4);
        let c = parse(&glb).unwrap();
        assert!(c.bin.is_empty());
    }

    #[test]
    fn header_errors() {
        assert_eq!(kind(b"nope"), ErrorKind::Magic);
        assert_eq!(kind(b"glTF"), ErrorKind::Magic);
        let mut glb = write(b"{}", &[]).unwrap();
        glb[4] = 1;
        assert_eq!(kind(&glb), ErrorKind::Version);
    }

    #[test]
    fn truncation_is_an_error_never_a_panic() {
        let glb = write(b"{}", &[9; 8]).unwrap();
        for cut in 0..glb.len() {
            assert!(parse(&glb[..cut]).is_err(), "cut at {cut} parsed");
        }
        // Header understating the length must not silently drop BIN.
        let mut short = glb.clone();
        short[8..12].copy_from_slice(&26u32.to_le_bytes());
        assert_eq!(kind(&short), ErrorKind::Truncated);
        // Lying chunk lengths.
        let mut lie = glb.clone();
        lie[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(kind(&lie), ErrorKind::Truncated);
    }

    #[test]
    fn first_chunk_must_be_json() {
        let mut glb = write(b"{}", &[]).unwrap();
        glb[16..20].copy_from_slice(&CHUNK_BIN.to_le_bytes());
        assert_eq!(kind(&glb), ErrorKind::Json);
    }

    #[test]
    fn unknown_chunks_skip_and_first_bin_wins() {
        let mut glb = write(b"{}", &[]).unwrap();
        for (kind, data) in [
            (0x1234_5678u32, [7u8; 4]),
            (CHUNK_BIN, [1; 4]),
            (CHUNK_BIN, [2; 4]),
        ] {
            glb.extend_from_slice(&4u32.to_le_bytes());
            glb.extend_from_slice(&kind.to_le_bytes());
            glb.extend_from_slice(&data);
        }
        let n = glb.len() as u32;
        glb[8..12].copy_from_slice(&n.to_le_bytes());
        assert_eq!(parse(&glb).unwrap().bin, &[1; 4]);
    }

    #[test]
    fn trailing_bytes_past_declared_length_are_ignored() {
        let mut glb = write(b"{}", &[5; 4]).unwrap();
        glb.extend_from_slice(b"junk");
        assert_eq!(parse(&glb).unwrap().bin, &[5; 4]);
    }
}
