// SPDX-License-Identifier: MIT
//! v4: mobile perf-budget checks (P_* layer). v5 adds per-class
//! budgets (--class), a bone budget, and asset-convention warnings
//! (normal maps, piece skinning, weapon shape).
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
use std::collections::BTreeSet;
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
/// Loose generic ceiling: 256 matrices x 64 B = 16 KB, exactly the
/// guaranteed UBO size — anything past this cannot upload its palette
/// everywhere. Per-class mobile budgets sit well below (see
/// `Budget::mobile_for`).
pub const D_MAX_BONES: usize = 256;

pub const BUDGET_KEYS: &str = "max_tris_per_mesh, max_verts_per_mesh, \
     max_texture_dim, max_influences, max_bones";

/// Asset class for per-class budgets (`--class`). Heroes are the
/// player cast, NPCs background humanoids, monsters creatures and
/// bosses, props static kit pieces, weapons hand-held props.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AssetClass {
    Hero,
    Npc,
    Monster,
    Prop,
    Weapon,
}

pub const CLASS_NAMES: &str = "hero, npc, monster, prop, weapon";

pub fn parse_class(name: &str) -> Option<AssetClass> {
    match name {
        "hero" => Some(AssetClass::Hero),
        "npc" => Some(AssetClass::Npc),
        "monster" => Some(AssetClass::Monster),
        "prop" => Some(AssetClass::Prop),
        "weapon" => Some(AssetClass::Weapon),
        _ => None,
    }
}

impl AssetClass {
    pub fn name(self) -> &'static str {
        match self {
            AssetClass::Hero => "hero",
            AssetClass::Npc => "npc",
            AssetClass::Monster => "monster",
            AssetClass::Prop => "prop",
            AssetClass::Weapon => "weapon",
        }
    }

    /// Characters ship baked normal maps; props and weapons need not.
    pub fn is_character(self) -> bool {
        matches!(
            self,
            AssetClass::Hero | AssetClass::Npc | AssetClass::Monster
        )
    }

    /// Props and weapons are legitimately unrigged; characters must
    /// carry a skeleton (the rig contract's R_NO_SKIN).
    pub fn needs_skeleton(self) -> bool {
        !matches!(self, AssetClass::Prop | AssetClass::Weapon)
    }
}

/// Perf budget: every field is a "warn above this" ceiling.
pub struct Budget {
    pub max_tris_per_mesh: usize,
    pub max_verts_per_mesh: usize,
    pub max_texture_dim: u32,
    pub max_influences: usize,
    pub max_bones: usize,
}

impl Default for Budget {
    /// 2024-flagship mobile budget (Samsung Galaxy S24 class floor).
    fn default() -> Self {
        Budget {
            max_tris_per_mesh: D_MAX_TRIS_PER_MESH,
            max_verts_per_mesh: D_MAX_VERTS_PER_MESH,
            max_texture_dim: D_MAX_TEXTURE_DIM,
            max_influences: D_MAX_INFLUENCES,
            max_bones: D_MAX_BONES,
        }
    }
}

impl Budget {
    /// Mobile-profile budget for one asset class: HLL art direction
    /// (heroes 5k-15k tris, 512-1024 px textures) tightened for
    /// phone-class GPUs. NPCs wrap the hero base today, so they share
    /// its bone budget until the lighter mobile rig lands; monsters
    /// may exceed heroes (one large boss draw, not a crowd); props
    /// and weapons are small static draws.
    pub fn mobile_for(class: AssetClass) -> Self {
        match class {
            AssetClass::Hero => Budget {
                max_tris_per_mesh: 15_000,
                max_verts_per_mesh: 10_000,
                max_texture_dim: 1024,
                max_influences: 4,
                max_bones: 128,
            },
            AssetClass::Npc => Budget {
                max_tris_per_mesh: 8_000,
                max_verts_per_mesh: 6_000,
                max_texture_dim: 512,
                max_influences: 4,
                max_bones: 128,
            },
            AssetClass::Monster => Budget {
                max_tris_per_mesh: 20_000,
                max_verts_per_mesh: 12_000,
                max_texture_dim: 1024,
                max_influences: 4,
                max_bones: 128,
            },
            AssetClass::Prop => Budget {
                max_tris_per_mesh: 2_000,
                max_verts_per_mesh: 1_500,
                max_texture_dim: 512,
                max_influences: 4,
                max_bones: 64,
            },
            AssetClass::Weapon => Budget {
                max_tris_per_mesh: 2_000,
                max_verts_per_mesh: 1_500,
                max_texture_dim: 512,
                max_influences: 4,
                max_bones: 16,
            },
        }
    }

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
            "max_bones" => {
                self.max_bones =
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

/// One budget file: top-level (generic) overrides plus per-class
/// `[class.X]` overrides. A class resolves each key from its section
/// first, then the file top level, then its mobile-profile default —
/// so an old flat file keeps meaning "every asset", and a section
/// only narrows its own class.
pub struct BudgetSet {
    pub generic: Budget,
    hero: Budget,
    npc: Budget,
    monster: Budget,
    prop: Budget,
    weapon: Budget,
}

impl BudgetSet {
    pub fn mobile() -> Self {
        BudgetSet {
            generic: Budget::default(),
            hero: Budget::mobile_for(AssetClass::Hero),
            npc: Budget::mobile_for(AssetClass::Npc),
            monster: Budget::mobile_for(AssetClass::Monster),
            prop: Budget::mobile_for(AssetClass::Prop),
            weapon: Budget::mobile_for(AssetClass::Weapon),
        }
    }

    pub fn for_class(&self, class: Option<AssetClass>) -> &Budget {
        match class {
            None => &self.generic,
            Some(AssetClass::Hero) => &self.hero,
            Some(AssetClass::Npc) => &self.npc,
            Some(AssetClass::Monster) => &self.monster,
            Some(AssetClass::Prop) => &self.prop,
            Some(AssetClass::Weapon) => &self.weapon,
        }
    }

    fn class_mut(&mut self, class: AssetClass) -> &mut Budget {
        match class {
            AssetClass::Hero => &mut self.hero,
            AssetClass::Npc => &mut self.npc,
            AssetClass::Monster => &mut self.monster,
            AssetClass::Prop => &mut self.prop,
            AssetClass::Weapon => &mut self.weapon,
        }
    }
}

/// Raw overrides from one budget file: top-level (generic) keys
/// plus per-class section keys, applied by `build` in increasing
/// specificity (mobile default < top level < class section).
struct FileBudget {
    top: Vec<(String, u64)>,
    sections: Vec<(AssetClass, String, u64)>,
}

impl FileBudget {
    fn build(self) -> Result<BudgetSet, String> {
        let mut set = BudgetSet::mobile();
        for (k, n) in &self.top {
            set.generic.set(k, *n)?;
        }
        // Top-level keys are the fallback for every class...
        for c in [
            AssetClass::Hero,
            AssetClass::Npc,
            AssetClass::Monster,
            AssetClass::Prop,
            AssetClass::Weapon,
        ] {
            for (k, n) in &self.top {
                set.class_mut(c).set(k, *n)?;
            }
        }
        // ...and a class section narrows only its own class.
        for (c, k, n) in &self.sections {
            set.class_mut(*c).set(k, *n)?;
        }
        Ok(set)
    }
}

fn parse_json_budget(text: &str) -> Result<FileBudget, String> {
    let v: Value =
        serde_json::from_str(text).map_err(|e| format!("budget JSON does not parse: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| "budget JSON must be an object".to_string())?;
    let mut fb = FileBudget {
        top: Vec::new(),
        sections: Vec::new(),
    };
    for (k, v) in obj {
        if k == "class" {
            let classes = v
                .as_object()
                .ok_or_else(|| "budget key 'class' must be an object".to_string())?;
            for (cn, cv) in classes {
                let class = parse_class(cn).ok_or_else(|| {
                    format!("unknown asset class '{cn}' (expected {CLASS_NAMES})")
                })?;
                let keys = cv
                    .as_object()
                    .ok_or_else(|| format!("budget class '{cn}' must be an object"))?;
                for (kk, vv) in keys {
                    let n = vv.as_u64().ok_or_else(|| {
                        format!("budget class '{cn}' key '{kk}': expected a non-negative integer")
                    })?;
                    Budget::default()
                        .set(kk, n)
                        .map_err(|e| format!("budget class '{cn}': {e}"))?;
                    fb.sections.push((class, kk.clone(), n));
                }
            }
            continue;
        }
        let n = v
            .as_u64()
            .ok_or_else(|| format!("budget key '{k}': expected a non-negative integer"))?;
        Budget::default().set(k, n)?;
        fb.top.push((k.clone(), n));
    }
    Ok(fb)
}

/// Flat `key = value` subset plus `[class.X]` sections: blank lines,
/// `#` comments (full-line or trailing), unsigned integers. No other
/// sections, no tables, no strings — a budget file holds numbers, and
/// anything else is a typo.
fn parse_toml_budget(text: &str) -> Result<FileBudget, String> {
    let mut fb = FileBudget {
        top: Vec::new(),
        sections: Vec::new(),
    };
    let mut section: Option<AssetClass> = None;
    for (ln, raw) in text.lines().enumerate() {
        let tag = format!("budget line {}", ln + 1);
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let inner = line
                .strip_suffix(']')
                .ok_or_else(|| format!("{tag}: malformed [section] (missing ']')"))?;
            let inner = inner[1..].trim();
            let cn = inner.strip_prefix("class.").ok_or_else(|| {
                format!("{tag}: [{inner}] unsupported (only [class.X], X in {CLASS_NAMES})")
            })?;
            let cn = cn.trim();
            section = Some(parse_class(cn).ok_or_else(|| {
                format!("{tag}: unknown asset class '{cn}' (expected {CLASS_NAMES})")
            })?);
            continue;
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
        Budget::default()
            .set(key, n)
            .map_err(|e| format!("{tag}: {e}"))?;
        match section {
            None => fb.top.push((key.to_string(), n)),
            Some(c) => fb.sections.push((c, key.to_string(), n)),
        }
    }
    Ok(fb)
}

/// Parse budget overrides; format is sniffed from content (a leading
/// `{` means JSON, anything else the TOML subset). Missing keys keep
/// their mobile defaults (generic or per-class).
pub fn parse_budget_set(text: &str) -> Result<BudgetSet, String> {
    let fb = if text.trim_start().starts_with('{') {
        parse_json_budget(text)
    } else {
        parse_toml_budget(text)
    }?;
    fb.build()
}

pub fn load_budget_set(path: &Path) -> Result<BudgetSet, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("cannot read budget file: {e}"))?;
    parse_budget_set(&text)
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

/// "hero " for classed runs, "" unclassed: classed warns name the
/// class whose budget fired ("exceed 15000 hero budget").
fn class_tag(class: Option<AssetClass>) -> String {
    match class {
        Some(c) => format!("{} ", c.name()),
        None => String::new(),
    }
}

fn join_names(names: &[String]) -> String {
    const CAP: usize = 5;
    if names.len() <= CAP {
        names.join(", ")
    } else {
        format!("{} (+{} more)", names[..CAP].join(", "), names.len() - CAP)
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
fn check_meshes(
    json: &Value,
    b: &Budget,
    class: Option<AssetClass>,
    out: &mut Vec<(&'static str, String)>,
) {
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
                    "{mtag}: {verts} verts exceed {} {}budget{}",
                    b.max_verts_per_mesh,
                    class_tag(class),
                    note(v_unknown),
                ),
            ));
        }
        if tris > b.max_tris_per_mesh {
            out.push((
                "P_TRIS",
                format!(
                    "{mtag}: {tris} tris exceed {} {}budget{}",
                    b.max_tris_per_mesh,
                    class_tag(class),
                    note(t_unknown),
                ),
            ));
        }
    }
}

/// Embedded (bufferView) texture dims; external/data URIs and foreign
/// codecs are skipped silently (unmeasured, not over-budget).
fn check_textures(
    json: &Value,
    bin: &[u8],
    b: &Budget,
    class: Option<AssetClass>,
    out: &mut Vec<(&'static str, String)>,
) {
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
                    "{}: {w}x{h} exceeds {}px {}budget",
                    img_tag(images, ii),
                    b.max_texture_dim,
                    class_tag(class),
                ),
            ));
        }
    }
}

/// Unique DEF-* joint nodes across all skins: the deform-bone
/// count the skinning palette must upload. Non-DEF joints and
/// dangling refs are the contract layer's business (R_*), not the
/// budget's.
fn deform_bone_count(json: &Value) -> usize {
    let nodes = arr(json, "nodes");
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    for skin in arr(json, "skins") {
        for j in arr(skin, "joints") {
            let idx = j.as_u64().and_then(|n| usize::try_from(n).ok());
            let is_def = idx.is_some_and(|i| {
                nodes
                    .get(i)
                    .and_then(|n| n.get("name"))
                    .and_then(Value::as_str)
                    .is_some_and(|n| n.starts_with("DEF-"))
            });
            if is_def {
                seen.insert(idx.unwrap_or(usize::MAX));
            }
        }
    }
    seen.len()
}

fn check_bones(
    json: &Value,
    b: &Budget,
    class: Option<AssetClass>,
    out: &mut Vec<(&'static str, String)>,
) {
    let n = deform_bone_count(json);
    if n > b.max_bones {
        out.push((
            "P_BONES",
            format!(
                "{n} deform bone{} exceed{} {} {}budget",
                if n == 1 { "" } else { "s" },
                if n == 1 { "s" } else { "" },
                b.max_bones,
                class_tag(class),
            ),
        ));
    }
}

/// Characters ship baked normal maps (AI color/detail is baked onto
/// the low-poly); a character class with no material carrying
/// `normalTexture` warns. Props and weapons need none, and unclassed
/// runs cannot know the file is a character, so both skip.
fn check_normal_map(
    json: &Value,
    class: Option<AssetClass>,
    out: &mut Vec<(&'static str, String)>,
) {
    let Some(c) = class else { return };
    if !c.is_character() {
        return;
    }
    let has_normal = arr(json, "materials")
        .iter()
        .any(|m| m.get("normalTexture").is_some());
    if !has_normal {
        out.push((
            "P_TEX_NORMAL",
            format!(
                "no material with normalTexture ({} needs a baked normal map)",
                c.name(),
            ),
        ));
    }
}

/// Detachable pieces (capes, hair) are remeshed separately and get
/// body weights via Data Transfer, so every bone a piece mesh uses
/// must also carry body weight. Detection is by mesh name
/// (case-insensitive); body is the union of nonzero-weight joints
/// over all other skinned meshes. Piece-only files, unskinned
/// pieces, and unreadable weights skip silently (nothing provable;
/// the weight layer owns malformed reads).
const PIECE_WORDS: &[&str] = &["cape", "cloak", "hair", "ponytail", "braid"];

fn is_piece_mesh(mesh: &Value) -> bool {
    let name = mesh.get("name").and_then(Value::as_str).unwrap_or("");
    let lower = name.to_lowercase();
    PIECE_WORDS.iter().any(|w| lower.contains(w))
}

fn check_pieces(json: &Value, bin: &[u8], out: &mut Vec<(&'static str, String)>) {
    let meshes = arr(json, "meshes");
    let mut body: BTreeSet<usize> = BTreeSet::new();
    let mut pieces: Vec<(usize, BTreeSet<usize>)> = Vec::new();
    for (mi, mesh) in meshes.iter().enumerate() {
        let used = match crate::weights::used_nodes(json, bin, mi) {
            Some(u) => u,
            None => continue,
        };
        if is_piece_mesh(mesh) {
            pieces.push((mi, used));
        } else {
            body.extend(used);
        }
    }
    if pieces.is_empty() || body.is_empty() {
        return;
    }
    let nodes = arr(json, "nodes");
    for (mi, used) in pieces {
        let mut bad: Vec<String> = used
            .difference(&body)
            .filter_map(|n| {
                nodes
                    .get(*n)
                    .and_then(|x| x.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        bad.sort();
        bad.dedup();
        if !bad.is_empty() {
            out.push((
                "P_PIECE_BONES",
                format!(
                    "{}: skinned to {} non-body bone{}: {}",
                    mesh_tag(meshes, mi),
                    bad.len(),
                    if bad.len() == 1 { "" } else { "s" },
                    join_names(&bad),
                ),
            ));
        }
    }
}

/// Weapons attach to the hand bone in Godot (`BoneAttachment3D`):
/// the file carries no skin, and a node named `ATTACH-*` marks the
/// grip point the importer snaps to the hand. Class-gated: only a
/// `--class weapon` run knows the file is a weapon.
fn check_weapon(json: &Value, class: Option<AssetClass>, out: &mut Vec<(&'static str, String)>) {
    if class != Some(AssetClass::Weapon) {
        return;
    }
    let n = arr(json, "skins").len();
    if n > 0 {
        out.push((
            "P_WEAPON_SKIN",
            format!(
                "{n} skin{} (weapons attach, never skin)",
                if n == 1 { "" } else { "s" },
            ),
        ));
    }
    let has_attach = arr(json, "nodes").iter().any(|nd| {
        nd.get("name")
            .and_then(Value::as_str)
            .is_some_and(|nm| nm.starts_with("ATTACH-"))
    });
    if !has_attach {
        out.push((
            "P_WEAPON_ATTACH",
            "no ATTACH-* node (weapons need a named grip point)".to_string(),
        ));
    }
}

pub fn check_budgets(
    json: &Value,
    bin: &[u8],
    budget: &Budget,
    class: Option<AssetClass>,
    skin: &SkinStats,
) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    check_meshes(json, budget, class, &mut out);
    check_textures(json, bin, budget, class, &mut out);
    check_bones(json, budget, class, &mut out);
    check_normal_map(json, class, &mut out);
    check_pieces(json, bin, &mut out);
    check_weapon(json, class, &mut out);
    // Reuses the weight layer's stats: prims it could not read (sparse,
    // malformed) contribute nothing here either.
    if skin.checked_prims > 0 && skin.max_influences > budget.max_influences {
        out.push((
            "P_INFLUENCES",
            format!(
                "max {} infl/vert exceeds {} {}budget (across {} checked prim{})",
                skin.max_influences,
                budget.max_influences,
                class_tag(class),
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

    /// Generic budget from a budget file: class sections parse but
    /// are dropped here (class tests use `parse_budget_set`).
    fn parse_budget(text: &str) -> Result<Budget, String> {
        Ok(parse_budget_set(text)?.generic)
    }

    #[test]
    fn defaults_match_readme() {
        let b = Budget::default();
        assert_eq!(b.max_tris_per_mesh, 100_000);
        assert_eq!(b.max_verts_per_mesh, 65_535);
        assert_eq!(b.max_texture_dim, 2048);
        assert_eq!(b.max_influences, 4);
        assert_eq!(b.max_bones, 256);
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

    #[test]
    fn classes_parse_and_group() {
        use AssetClass::*;
        assert_eq!(parse_class("hero"), Some(Hero));
        assert_eq!(parse_class("npc"), Some(Npc));
        assert_eq!(parse_class("monster"), Some(Monster));
        assert_eq!(parse_class("prop"), Some(Prop));
        assert_eq!(parse_class("weapon"), Some(Weapon));
        assert_eq!(parse_class("boss"), None);
        assert_eq!(parse_class("Hero"), None);
        assert_eq!(parse_class(""), None);
        assert!(Hero.is_character() && Npc.is_character() && Monster.is_character());
        assert!(!Prop.is_character() && !Weapon.is_character());
        assert!(Hero.needs_skeleton() && Npc.needs_skeleton() && Monster.needs_skeleton());
        assert!(!Prop.needs_skeleton() && !Weapon.needs_skeleton());
    }

    fn ceilings(b: &Budget) -> (usize, usize, u32, usize, usize) {
        (
            b.max_tris_per_mesh,
            b.max_verts_per_mesh,
            b.max_texture_dim,
            b.max_influences,
            b.max_bones,
        )
    }

    #[test]
    fn mobile_class_defaults_match_readme() {
        use AssetClass::*;
        assert_eq!(
            ceilings(&Budget::mobile_for(Hero)),
            (15_000, 10_000, 1024, 4, 128)
        );
        assert_eq!(
            ceilings(&Budget::mobile_for(Npc)),
            (8_000, 6_000, 512, 4, 128)
        );
        assert_eq!(
            ceilings(&Budget::mobile_for(Monster)),
            (20_000, 12_000, 1024, 4, 128)
        );
        assert_eq!(
            ceilings(&Budget::mobile_for(Prop)),
            (2_000, 1_500, 512, 4, 64)
        );
        assert_eq!(
            ceilings(&Budget::mobile_for(Weapon)),
            (2_000, 1_500, 512, 4, 16)
        );
    }

    #[test]
    fn toml_class_sections_fall_back() {
        use AssetClass::*;
        let set = parse_budget_set(
            "max_tris_per_mesh = 50000\n\
             [class.hero]\n\
             max_tris_per_mesh = 15000\n\
             max_bones = 100\n",
        )
        .unwrap();
        // Generic sees only the top level.
        assert_eq!(set.generic.max_tris_per_mesh, 50_000);
        assert_eq!(set.generic.max_bones, D_MAX_BONES);
        // Section narrows its own class...
        let hero = set.for_class(Some(Hero));
        assert_eq!(hero.max_tris_per_mesh, 15_000);
        assert_eq!(hero.max_bones, 100);
        assert_eq!(hero.max_texture_dim, 1024);
        // Other classes fall back to the top level, then their
        // mobile defaults; hero keeps its own mobile default above.
        let npc = set.for_class(Some(Npc));
        assert_eq!(npc.max_tris_per_mesh, 50_000);
        assert_eq!(npc.max_bones, 128);
        assert_eq!(npc.max_texture_dim, 512);
        assert!(set.for_class(None).max_tris_per_mesh == 50_000);
    }

    #[test]
    fn toml_class_rejects_garbage() {
        assert!(parse_budget_set("[class.boss]\nmax_tris_per_mesh = 5").is_err());
        assert!(parse_budget_set("[budget]\nmax_tris_per_mesh = 5").is_err());
        assert!(parse_budget_set("[class.hero\nmax_tris_per_mesh = 5").is_err());
        assert!(parse_budget_set("[class.hero]\nmax_tris = 5").is_err());
        assert!(parse_budget_set("[class.hero]\nmax_bones = lots").is_err());
        assert!(parse_budget_set("[class.]\nmax_bones = 5").is_err());
        // Sections do not leak into the generic budget.
        let set = parse_budget_set("[class.hero]\nmax_bones = 100\n").unwrap();
        assert_eq!(set.generic.max_bones, D_MAX_BONES);
        assert_eq!(set.for_class(Some(AssetClass::Hero)).max_bones, 100);
    }

    #[test]
    fn json_class_nesting_falls_back() {
        use AssetClass::*;
        let set = parse_budget_set(
            r#"{"max_texture_dim": 1024,
                "class": {"prop": {"max_tris_per_mesh": 1000},
                          "hero": {"max_bones": 100}}}"#,
        )
        .unwrap();
        assert_eq!(set.generic.max_texture_dim, 1024);
        assert_eq!(set.generic.max_tris_per_mesh, D_MAX_TRIS_PER_MESH);
        let prop = set.for_class(Some(Prop));
        assert_eq!(prop.max_tris_per_mesh, 1_000);
        assert_eq!(prop.max_texture_dim, 1024); // top-level fallback
        let hero = set.for_class(Some(Hero));
        assert_eq!(hero.max_bones, 100);
        assert_eq!(hero.max_texture_dim, 1024); // top-level beats class default
        assert_eq!(hero.max_tris_per_mesh, 15_000); // hero default kept
    }

    #[test]
    fn json_class_rejects_garbage() {
        assert!(parse_budget_set(r#"{"class": {"boss": {}}}"#).is_err());
        assert!(parse_budget_set(r#"{"class": {"hero": {"max_tris": 1}}}"#).is_err());
        assert!(parse_budget_set(r#"{"class": 5}"#).is_err());
        assert!(parse_budget_set(r#"{"class": {"hero": 5}}"#).is_err());
        assert!(parse_budget_set(r#"{"class": {"hero": {"max_bones": -1}}}"#).is_err());
    }
}
