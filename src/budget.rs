// SPDX-License-Identifier: MIT
//! v4: mobile perf-budget checks (P_* layer).
//!
//! Advisory only: over-budget assets WARN, never fail. Correctness
//! (R_*/W_*/X_*) stays FAIL-level; budgets answer "will this hurt on
//! a phone", not "is this broken". Like X_*, this layer only reports
//! what it can prove: a mesh warns only when its known counts already
//! exceed the budget (unknown prims can only add), and anything
//! unreadable (dangling refs, foreign image codecs, external URIs) is
//! skipped silently.

use crate::weights::SkinStats;
use serde_json::Value;
use std::fs;
use std::path::Path;

/// Single character draw call on a 2024 flagship: engines ship
/// mobile heroes at 50-100k tris (Unity URP / UE5 mobile guidance).
pub const D_MAX_TRIS_PER_MESH: usize = 100_000;
/// Order of the 16-bit index ceiling: past 65,535 verts a mesh cannot
/// draw in one UINT16-indexed call, so engines split prims or widen
/// to 32-bit indices — both cost on tile-based mobile GPUs.
pub const D_MAX_VERTS_PER_MESH: usize = 65_535;
/// Largest single texture on a mobile hero: a 4k RGBA costs 16 MB
/// even ASTC-compressed (8bpp); 2k is standard phone practice.
pub const D_MAX_TEXTURE_DIM: u32 = 2048;
/// Same as the rig contract: 4-bone skinning is the mobile GPU
/// standard (Adreno/Xclipse/Mali handle it natively). Tighten via
/// --budget for low-end targets.
pub const D_MAX_INFLUENCES: usize = 4;

pub const BUDGET_KEYS: &str =
    "max_tris_per_mesh, max_verts_per_mesh, max_texture_dim, max_influences";

/// Perf budget: every field is a "warn above this" ceiling.
pub struct Budget {
    pub max_tris_per_mesh: usize,
    pub max_verts_per_mesh: usize,
    pub max_texture_dim: u32,
    pub max_influences: usize,
}

impl Default for Budget {
    /// 2024-flagship mobile budget (Samsung Galaxy S24 class floor).
    fn default() -> Self {
        Budget {
            max_tris_per_mesh: D_MAX_TRIS_PER_MESH,
            max_verts_per_mesh: D_MAX_VERTS_PER_MESH,
            max_texture_dim: D_MAX_TEXTURE_DIM,
            max_influences: D_MAX_INFLUENCES,
        }
    }
}

impl Budget {
    fn set(&mut self, key: &str, n: u64) -> Result<(), String> {
        match key {
            "max_tris_per_mesh" => {
                self.max_tris_per_mesh =
                    usize::try_from(n).map_err(|_| format!("budget key '{key}': {n} overflows"))?;
            }
            "max_verts_per_mesh" => {
                self.max_verts_per_mesh =
                    usize::try_from(n).map_err(|_| format!("budget key '{key}': {n} overflows"))?;
            }
            "max_texture_dim" => {
                self.max_texture_dim = u32::try_from(n)
                    .map_err(|_| format!("budget key '{key}': {n} overflows u32"))?;
            }
            "max_influences" => {
                self.max_influences =
                    usize::try_from(n).map_err(|_| format!("budget key '{key}': {n} overflows"))?;
            }
            _ => {
                return Err(format!(
                    "unknown budget key '{key}' (expected {BUDGET_KEYS})"
                ))
            }
        }
        Ok(())
    }
}

fn parse_json_budget(text: &str) -> Result<Budget, String> {
    let v: Value =
        serde_json::from_str(text).map_err(|e| format!("budget JSON does not parse: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| "budget JSON must be an object".to_string())?;
    let mut b = Budget::default();
    for (k, v) in obj {
        let n = v
            .as_u64()
            .ok_or_else(|| format!("budget key '{k}': expected a non-negative integer"))?;
        b.set(k, n)?;
    }
    Ok(b)
}

/// Flat `key = value` subset: blank lines, `#` comments (full-line or
/// trailing), unsigned integers. No sections, no tables, no strings —
/// a budget file holds four numbers, and anything else is a typo.
fn parse_toml_budget(text: &str) -> Result<Budget, String> {
    let mut b = Budget::default();
    for (ln, raw) in text.lines().enumerate() {
        let tag = format!("budget line {}", ln + 1);
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            return Err(format!(
                "{tag}: [sections] unsupported (flat key = value only)"
            ));
        }
        let (k, v) = raw
            .split_once('=')
            .ok_or_else(|| format!("{tag}: expected key = value"))?;
        let key = k.trim();
        let val = v.split('#').next().unwrap_or("").trim();
        if key.is_empty() || val.is_empty() {
            return Err(format!("{tag}: expected key = value"));
        }
        if !val.bytes().all(|c| c.is_ascii_digit()) {
            return Err(format!(
                "{tag}: value for '{key}' must be a non-negative integer"
            ));
        }
        let n: u64 = val
            .parse()
            .map_err(|_| format!("{tag}: value for '{key}' overflows"))?;
        b.set(key, n).map_err(|e| format!("{tag}: {e}"))?;
    }
    Ok(b)
}

/// Parse budget overrides; format is sniffed from content (a leading
/// `{` means JSON, anything else the flat TOML subset). Missing keys
/// keep their mobile defaults.
pub fn parse_budget(text: &str) -> Result<Budget, String> {
    if text.trim_start().starts_with('{') {
        parse_json_budget(text)
    } else {
        parse_toml_budget(text)
    }
}

pub fn load_budget(path: &Path) -> Result<Budget, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("cannot read budget file: {e}"))?;
    parse_budget(&text)
}

fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn as_idx(v: &Value) -> Option<usize> {
    v.as_u64().and_then(|n| usize::try_from(n).ok())
}

fn as_usize(v: &Value, key: &str) -> Option<usize> {
    v.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

fn acc_count(json: &Value, ai: usize) -> Option<usize> {
    arr(json, "accessors")
        .get(ai)
        .and_then(|a| as_usize(a, "count"))
}

fn mesh_tag(meshes: &[Value], mi: usize) -> String {
    match meshes
        .get(mi)
        .and_then(|m| m.get("name"))
        .and_then(Value::as_str)
    {
        Some(n) if !n.is_empty() => format!("mesh {mi} '{n}'"),
        _ => format!("mesh {mi}"),
    }
}

fn img_tag(images: &[Value], ii: usize) -> String {
    match images
        .get(ii)
        .and_then(|m| m.get("name"))
        .and_then(Value::as_str)
    {
        Some(n) if !n.is_empty() => format!("image {ii} '{n}'"),
        _ => format!("image {ii}"),
    }
}

/// Image dims live in the codec header, not the glTF JSON, so the
/// budget layer sniffs the three codecs game GLBs ship: PNG, JPEG,
/// KTX2. Bounds-checked; None means truncated, corrupt, or foreign
/// (skip silently — an unmeasured texture is not an over-budget one).
/// Implausible dims (> 64k a side; real GPUs cap at 16k) count as
/// corrupt, so garbage headers can never warn spuriously.
fn sane_dims(w: u32, h: u32) -> Option<(u32, u32)> {
    if w == 0 || h == 0 || w > 65536 || h > 65536 {
        None
    } else {
        Some((w, h))
    }
}

fn png_dims(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 24 || &b[0..8] != b"\x89PNG\r\n\x1a\n" || &b[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(b[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(b[20..24].try_into().ok()?);
    sane_dims(w, h)
}

fn jpeg_dims(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return None;
    }
    let mut at = 2usize;
    while at + 1 < b.len() {
        if b[at] != 0xFF {
            return None;
        }
        while at < b.len() && b[at] == 0xFF {
            at += 1;
        }
        if at >= b.len() {
            return None;
        }
        let m = b[at];
        at += 1;
        if m == 0xD8 || m == 0x01 || (0xD0..=0xD7).contains(&m) {
            continue; // standalone markers (no length field)
        }
        if m == 0xD9 || m == 0xDA {
            return None; // EOI / start-of-scan: no SOF follows
        }
        if at + 1 >= b.len() {
            return None;
        }
        let len = u16::from_be_bytes([b[at], b[at + 1]]) as usize;
        if len < 2 || at + len > b.len() {
            return None;
        }
        let is_sof = matches!(m, 0xC0..=0xCF) && !matches!(m, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            if len < 7 {
                return None;
            }
            let h = u16::from_be_bytes([b[at + 3], b[at + 4]]) as u32;
            let w = u16::from_be_bytes([b[at + 5], b[at + 6]]) as u32;
            return sane_dims(w, h);
        }
        at += len;
    }
    None
}

const KTX2_MAGIC: &[u8; 12] = b"\xabKTX 20\xbb\r\n\x1a\n";

fn ktx2_dims(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 28 || &b[0..12] != KTX2_MAGIC {
        return None;
    }
    let w = u32::from_le_bytes(b[20..24].try_into().ok()?);
    let h = u32::from_le_bytes(b[24..28].try_into().ok()?);
    sane_dims(w, h)
}

fn image_bytes<'a>(json: &Value, bin: &'a [u8], bvi: usize) -> Option<&'a [u8]> {
    let bv = arr(json, "bufferViews").get(bvi)?;
    if bv.get("buffer").and_then(Value::as_u64).unwrap_or(0) != 0 {
        return None;
    }
    let off = bv.get("byteOffset").and_then(as_idx).unwrap_or(0);
    let len = as_usize(bv, "byteLength")?;
    bin.get(off..off.checked_add(len)?)
}

/// Per-mesh verts (POSITION counts) and tris (indexed count/3, else
/// verts/3 for triangle lists). Non-triangle modes carry no
/// triangle-list cost and are excluded from tris; prims whose counts
/// are unreadable are excluded from both — but a mesh still warns
/// when its known counts alone exceed the budget, since unknown
/// prims can only add (the `+N uncounted` note says so).
fn check_meshes(json: &Value, b: &Budget, out: &mut Vec<(&'static str, String)>) {
    for (mi, mesh) in arr(json, "meshes").iter().enumerate() {
        let mtag = mesh_tag(arr(json, "meshes"), mi);
        let mut verts = 0usize;
        let mut v_unknown = 0usize;
        let mut tris = 0usize;
        let mut t_unknown = 0usize;
        for prim in arr(mesh, "primitives") {
            let mode = prim.get("mode").and_then(Value::as_u64).unwrap_or(4);
            let vcount = prim
                .get("attributes")
                .and_then(|a| a.get("POSITION"))
                .and_then(as_idx)
                .and_then(|ai| acc_count(json, ai));
            match vcount {
                Some(v) => verts = verts.saturating_add(v),
                None => v_unknown += 1,
            }
            if mode != 4 {
                continue;
            }
            match prim
                .get("indices")
                .and_then(as_idx)
                .and_then(|ai| acc_count(json, ai))
            {
                Some(ic) => tris = tris.saturating_add(ic / 3),
                None => match vcount {
                    Some(v) => tris = tris.saturating_add(v / 3),
                    None => t_unknown += 1,
                },
            }
        }
        let note = |u: usize| {
            if u == 0 {
                String::new()
            } else {
                format!(" (+{u} uncounted prim{})", if u == 1 { "" } else { "s" })
            }
        };
        if verts > b.max_verts_per_mesh {
            out.push((
                "P_VERTS",
                format!(
                    "{mtag}: {verts} verts exceed {} budget{}",
                    b.max_verts_per_mesh,
                    note(v_unknown),
                ),
            ));
        }
        if tris > b.max_tris_per_mesh {
            out.push((
                "P_TRIS",
                format!(
                    "{mtag}: {tris} tris exceed {} budget{}",
                    b.max_tris_per_mesh,
                    note(t_unknown),
                ),
            ));
        }
    }
}

/// Embedded (bufferView) texture dims; external/data URIs and foreign
/// codecs are skipped silently (unmeasured, not over-budget).
fn check_textures(json: &Value, bin: &[u8], b: &Budget, out: &mut Vec<(&'static str, String)>) {
    let images = arr(json, "images");
    for (ii, im) in images.iter().enumerate() {
        let bytes = match im.get("bufferView").and_then(as_idx) {
            Some(bvi) => image_bytes(json, bin, bvi),
            None => None,
        };
        let bytes = match bytes {
            Some(x) => x,
            None => continue,
        };
        let dims = png_dims(bytes)
            .or_else(|| jpeg_dims(bytes))
            .or_else(|| ktx2_dims(bytes));
        let (w, h) = match dims {
            Some(d) => d,
            None => continue,
        };
        if w.max(h) > b.max_texture_dim {
            out.push((
                "P_TEX_SIZE",
                format!(
                    "{}: {w}x{h} exceeds {}px budget",
                    img_tag(images, ii),
                    b.max_texture_dim,
                ),
            ));
        }
    }
}

pub fn check_budgets(
    json: &Value,
    bin: &[u8],
    budget: &Budget,
    skin: &SkinStats,
) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    check_meshes(json, budget, &mut out);
    check_textures(json, bin, budget, &mut out);
    // Reuses the weight layer's stats: prims it could not read (sparse,
    // malformed) contribute nothing here either.
    if skin.checked_prims > 0 && skin.max_influences > budget.max_influences {
        out.push((
            "P_INFLUENCES",
            format!(
                "max {} infl/vert exceeds {} budget (across {} checked prim{})",
                skin.max_influences,
                budget.max_influences,
                skin.checked_prims,
                if skin.checked_prims == 1 { "" } else { "s" },
            ),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_readme() {
        let b = Budget::default();
        assert_eq!(b.max_tris_per_mesh, 100_000);
        assert_eq!(b.max_verts_per_mesh, 65_535);
        assert_eq!(b.max_texture_dim, 2048);
        assert_eq!(b.max_influences, 4);
    }

    #[test]
    fn json_overrides_subset() {
        let b = parse_budget(r#"{"max_tris_per_mesh": 10}"#).unwrap();
        assert_eq!(b.max_tris_per_mesh, 10);
        assert_eq!(b.max_verts_per_mesh, D_MAX_VERTS_PER_MESH);
        assert_eq!(b.max_texture_dim, D_MAX_TEXTURE_DIM);
        assert_eq!(b.max_influences, D_MAX_INFLUENCES);
        let b = parse_budget("{}").unwrap();
        assert_eq!(b.max_tris_per_mesh, D_MAX_TRIS_PER_MESH);
    }

    #[test]
    fn json_rejects_bad_keys_and_values() {
        assert!(parse_budget(r#"{"max_tris": 10}"#).is_err());
        assert!(parse_budget(r#"{"max_tris_per_mesh": 1.5}"#).is_err());
        assert!(parse_budget(r#"{"max_tris_per_mesh": -1}"#).is_err());
        assert!(parse_budget(r#"{"max_tris_per_mesh": "lots"}"#).is_err());
        assert!(parse_budget(r#"{"max_tris_per_mesh": true}"#).is_err());
        assert!(parse_budget(r#"{"max_tris_per_mesh": 10"#).is_err());
        assert!(parse_budget(r#"{"max_texture_dim": 4294967296}"#).is_err());
    }

    #[test]
    fn toml_overrides_subset() {
        let b = parse_budget(
            "# flagship-tightened\nmax_tris_per_mesh = 50000\nmax_texture_dim=512 # inline\n",
        )
        .unwrap();
        assert_eq!(b.max_tris_per_mesh, 50_000);
        assert_eq!(b.max_texture_dim, 512);
        assert_eq!(b.max_verts_per_mesh, D_MAX_VERTS_PER_MESH);
        assert_eq!(b.max_influences, D_MAX_INFLUENCES);
        let b = parse_budget("").unwrap();
        assert_eq!(b.max_tris_per_mesh, D_MAX_TRIS_PER_MESH);
        let b = parse_budget("  \n# only comments\n").unwrap();
        assert_eq!(b.max_texture_dim, D_MAX_TEXTURE_DIM);
    }

    #[test]
    fn toml_rejects_garbage() {
        assert!(parse_budget("max_tris = 10").is_err());
        assert!(parse_budget("max_tris_per_mesh = lots").is_err());
        assert!(parse_budget("max_tris_per_mesh = 1.5").is_err());
        assert!(parse_budget("max_tris_per_mesh = -5").is_err());
        assert!(parse_budget("max_tris_per_mesh = ").is_err());
        assert!(parse_budget("just words here").is_err());
        assert!(parse_budget("[budget]\nmax_tris_per_mesh = 5").is_err());
        assert!(parse_budget("max_tris_per_mesh = 99999999999999999999999").is_err());
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
        b.extend_from_slice(&13u32.to_be_bytes());
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 2, 0, 0, 0]); // bit depth etc. (unread)
        b
    }

    #[test]
    fn sniff_png() {
        assert_eq!(png_dims(&png(4096, 2048)), Some((4096, 2048)));
        assert_eq!(png_dims(&png(16, 16)), Some((16, 16)));
        assert_eq!(png_dims(&png(4096, 2048)[..20]), None); // truncated
        assert_eq!(png_dims(b"not a png at all................"), None);
        let mut bad = png(64, 64);
        bad[12..16].copy_from_slice(b"PLTE"); // first chunk not IHDR
        assert_eq!(png_dims(&bad), None);
    }

    #[test]
    fn sniff_jpeg() {
        // SOI + empty APP0 + SOF0 (32x16) + EOI.
        let mut b = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x02];
        b.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x10, 0x00, 0x20]);
        b.extend_from_slice(&[0x01, 0x01, 0x11, 0x00, 0xFF, 0xD9]);
        assert_eq!(jpeg_dims(&b), Some((32, 16)));
        assert_eq!(jpeg_dims(&b[..10]), None); // truncated mid-SOF
        assert_eq!(jpeg_dims(&[0xFF, 0xD8, 0xFF, 0xDA]), None); // SOS first
        assert_eq!(jpeg_dims(b"plain text, not jpeg...."), None);
    }

    #[test]
    fn sniff_ktx2() {
        let mut b = KTX2_MAGIC.to_vec();
        b.extend_from_slice(&0u32.to_le_bytes()); // vkFormat
        b.extend_from_slice(&1u32.to_le_bytes()); // typeSize
        b.extend_from_slice(&1024u32.to_le_bytes()); // pixelWidth
        b.extend_from_slice(&512u32.to_le_bytes()); // pixelHeight
        assert_eq!(ktx2_dims(&b), Some((1024, 512)));
        assert_eq!(ktx2_dims(&b[..20]), None);
        assert_eq!(ktx2_dims(b"KTX 11, not KTX2........"), None);
    }

    #[test]
    fn insane_dims_are_corrupt_not_over() {
        assert_eq!(png_dims(&png(100_000, 100)), None);
        assert_eq!(png_dims(&png(0, 64)), None);
        assert_eq!(sane_dims(65536, 65536), Some((65536, 65536)));
        assert_eq!(sane_dims(65537, 8), None);
    }
}
