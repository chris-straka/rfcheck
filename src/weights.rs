// SPDX-License-Identifier: MIT
//! v2: skin-weight stats via BIN accessor parsing.
//!
//! For every mesh bound to a skin, reads JOINTS_n/WEIGHTS_n accessors
//! out of the GLB BIN chunk and reports over-limit influence counts
//! and unnormalized weights. Anything unreadable (bad references,
//! out-of-bounds reads, sparse storage) is an explicit diagnostic,
//! never a silent skip or a panic.

use crate::util::{arr, as_idx, plural};
use serde_json::Value;
use std::collections::BTreeSet;

pub use glbkit::rig::{MAX_INFLUENCES, WEIGHT_TOL};

pub struct SkinStats {
    pub max_influences: usize,
    pub checked_prims: usize,
}

pub(crate) use glbkit::accessor::{Component, Layout};

/// Resolve an accessor to its BIN layout, bounds-checked. `what` names
/// the attribute for error detail (e.g. "WEIGHTS_0").
pub(crate) fn layout_of(
    json: &Value,
    bin_len: usize,
    ai: usize,
    what: &str,
) -> Result<Layout, String> {
    use glbkit::accessor::{Desc, LayoutError, View};
    let acc = arr(json, "accessors")
        .get(ai)
        .ok_or_else(|| format!("{what} accessor {ai} is out of range"))?;
    let bvi = acc
        .get("bufferView")
        .and_then(as_idx)
        .ok_or_else(|| format!("{what} accessor {ai} has no bufferView"))?;
    let bv = arr(json, "bufferViews")
        .get(bvi)
        .ok_or_else(|| format!("{what} accessor {ai} references missing bufferView {bvi}"))?;
    let num = |v: &Value, k: &str| v.get(k).and_then(as_idx);
    let byte_offset = num(bv, "byteOffset").unwrap_or(0);
    let view = View {
        buffer: num(bv, "buffer").unwrap_or(0),
        byte_offset,
        // byteLength is required; when absent, X_ACCESSOR_BOUNDS skips the
        // view and reads stay bounded by BIN alone.
        byte_length: num(bv, "byteLength").unwrap_or(bin_len.saturating_sub(byte_offset)),
        byte_stride: num(bv, "byteStride"),
    };
    let desc = Desc {
        count: num(acc, "count").unwrap_or(0),
        component_type: acc
            .get("componentType")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        kind: acc.get("type").and_then(Value::as_str).unwrap_or(""),
        normalized: acc
            .get("normalized")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        byte_offset: num(acc, "byteOffset").unwrap_or(0),
        view: Some(view),
    };
    let lay = Layout::resolve(&desc, bin_len).map_err(|e| match e {
        LayoutError::ExternalBuffer(_) => format!("{what} accessor {ai} uses a non-GLB buffer"),
        LayoutError::BadComponent(c) => format!("{what} accessor {ai} has componentType {c}"),
        LayoutError::Overflow => format!("{what} accessor {ai} extent overflows"),
        LayoutError::ViewPastBin { end, bin_len } => {
            format!("{what} accessor {ai} reads past the BIN chunk ({end} > {bin_len})")
        }
        LayoutError::PastView { end, view_end } => {
            format!("{what} accessor {ai} reads past bufferView {bvi} ({end} > {view_end})")
        }
        e => format!("{what} accessor {ai} has {e}"),
    })?;
    if lay.width > 4 {
        return Err(format!("{what} accessor {ai} has type {:?}", desc.kind));
    }
    Ok(lay)
}

/// Read one weight value with normalized-int conversion. Fully
/// bounds-checked; None means the layout lied (cannot happen after
/// `layout_of`, but corrupt input must never panic).
fn weight_at(bin: &[u8], lay: &Layout, v: usize, k: usize) -> Option<f64> {
    lay.read_weight(bin, v, k)
}

/// Raw f32 read (FLOAT accessors only). Bounds-checked; None on
/// any mismatch, so corrupt input can never panic.
pub(crate) fn f32_at(bin: &[u8], lay: &Layout, v: usize, k: usize) -> Option<f32> {
    lay.read_f32(bin, v, k)
}

/// Raw joint-index read (UNSIGNED_BYTE/SHORT accessors only).
/// Bounds-checked; None on any mismatch.
pub(crate) fn uint_at(bin: &[u8], lay: &Layout, v: usize, k: usize) -> Option<u32> {
    match lay.component {
        Component::U8 | Component::U16 => lay.read_uint(bin, v, k),
        _ => None,
    }
}

/// True for the JOINTS_n storage the spec allows: VEC4 UBYTE/USHORT.
fn joints_ok(lay: &Layout) -> bool {
    lay.width == 4 && matches!(lay.component, Component::U8 | Component::U16)
}

/// True for the WEIGHTS_n storage the spec allows: VEC4 FLOAT, or
/// normalized UBYTE/USHORT.
fn weights_ok(lay: &Layout) -> bool {
    lay.width == 4
        && (lay.component == Component::F32
            || (lay.normalized && matches!(lay.component, Component::U8 | Component::U16)))
}

pub fn check_weights(json: &Value, bin: &[u8]) -> (Vec<(&'static str, String)>, SkinStats) {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut stats = SkinStats {
        max_influences: 0,
        checked_prims: 0,
    };
    let meshes = arr(json, "meshes");
    let mut skinned: Vec<usize> = arr(json, "nodes")
        .iter()
        .filter(|n| n.get("skin").is_some())
        .filter_map(|n| n.get("mesh").and_then(as_idx))
        .filter(|mi| meshes.get(*mi).is_some())
        .collect();
    skinned.sort_unstable();
    skinned.dedup();
    for mi in skinned {
        let mesh = &meshes[mi];
        let mtag = match mesh.get("name").and_then(Value::as_str) {
            Some(n) if !n.is_empty() => format!("mesh {mi} '{n}'"),
            _ => format!("mesh {mi}"),
        };
        for (pi, prim) in arr(mesh, "primitives").iter().enumerate() {
            let ptag = format!("{mtag} prim {pi}");
            let attrs = prim.get("attributes");
            let acc_idx = |key: &str| attrs.and_then(|a| a.get(key)).and_then(as_idx);
            let (Some(j0), Some(w0)) = (acc_idx("JOINTS_0"), acc_idx("WEIGHTS_0")) else {
                out.push((
                    "W_BAD_ACCESSOR",
                    format!("{ptag}: skinned primitive lacks JOINTS_0/WEIGHTS_0"),
                ));
                continue;
            };
            let mut sets = vec![(j0, w0)];
            let mut broken = false;
            for n in 1.. {
                let jk = format!("JOINTS_{n}");
                let wk = format!("WEIGHTS_{n}");
                match (acc_idx(&jk), acc_idx(&wk)) {
                    (None, None) => break,
                    (Some(j), Some(w)) => sets.push((j, w)),
                    _ => {
                        out.push((
                            "W_BAD_ACCESSOR",
                            format!("{ptag}: {jk} without {wk} (or vice versa)"),
                        ));
                        broken = true;
                        break;
                    }
                }
            }
            if broken {
                continue;
            }
            let accs = arr(json, "accessors");
            let mut layouts: Vec<(Layout, Layout)> = Vec::new();
            let mut skipped = false;
            for (si, (j, w)) in sets.iter().enumerate() {
                let jt = format!("JOINTS_{si}");
                let wt = format!("WEIGHTS_{si}");
                for (tag, ai) in [(&jt, *j), (&wt, *w)] {
                    if accs.get(ai).and_then(|a| a.get("sparse")).is_some() {
                        out.push((
                            "W_SPARSE",
                            format!(
                                "{ptag}: {tag} accessor {ai} is sparse \
                                 (unsupported; weights unchecked)"
                            ),
                        ));
                        skipped = true;
                        break;
                    }
                }
                if skipped {
                    break;
                }
                let jl = match layout_of(json, bin.len(), *j, &jt) {
                    Ok(l) => l,
                    Err(e) => {
                        out.push(("W_BAD_ACCESSOR", format!("{ptag}: {e}")));
                        skipped = true;
                        break;
                    }
                };
                let wl = match layout_of(json, bin.len(), *w, &wt) {
                    Ok(l) => l,
                    Err(e) => {
                        out.push(("W_BAD_ACCESSOR", format!("{ptag}: {e}")));
                        skipped = true;
                        break;
                    }
                };
                if !joints_ok(&jl) {
                    out.push((
                        "W_BAD_ACCESSOR",
                        format!("{ptag}: {jt} accessor {j} must be VEC4 UNSIGNED_BYTE/SHORT"),
                    ));
                    skipped = true;
                    break;
                }
                let w_ok = weights_ok(&wl);
                if !w_ok {
                    out.push((
                        "W_BAD_ACCESSOR",
                        format!(
                            "{ptag}: {wt} accessor {w} must be VEC4 FLOAT \
                             or normalized UNSIGNED_BYTE/SHORT"
                        ),
                    ));
                    skipped = true;
                    break;
                }
                layouts.push((jl, wl));
            }
            if skipped {
                continue;
            }
            let count = layouts[0].1.count;
            if layouts
                .iter()
                .any(|(j, w)| j.count != count || w.count != count)
            {
                out.push((
                    "W_BAD_ACCESSOR",
                    format!("{ptag}: JOINTS/WEIGHTS accessor counts disagree"),
                ));
                continue;
            }
            let mut local_max = 0usize;
            let mut over = 0usize;
            let mut over_v = 0usize;
            let mut over_k = 0usize;
            let mut bad = 0usize;
            let mut bad_v = 0usize;
            let mut bad_sum = 0.0f64;
            let mut bad_dev = 0.0f64;
            let mut unreadable = false;
            for v in 0..count {
                let mut inf = 0usize;
                let mut sum = 0.0f64;
                for (_, wl) in &layouts {
                    for k in 0..4 {
                        match weight_at(bin, wl, v, k) {
                            Some(x) => {
                                if x != 0.0 {
                                    inf += 1;
                                }
                                sum += x;
                            }
                            None => {
                                unreadable = true;
                                break;
                            }
                        }
                    }
                    if unreadable {
                        break;
                    }
                }
                if unreadable {
                    break;
                }
                local_max = local_max.max(inf);
                if inf > MAX_INFLUENCES {
                    over += 1;
                    if inf > over_k {
                        over_k = inf;
                        over_v = v;
                    }
                }
                let dev = (sum - 1.0).abs();
                if dev.is_nan() || dev > WEIGHT_TOL {
                    bad += 1;
                    if bad == 1 || dev.is_nan() || dev > bad_dev {
                        bad_v = v;
                        bad_sum = sum;
                        bad_dev = dev;
                    }
                }
            }
            if unreadable {
                out.push(("W_BAD_ACCESSOR", format!("{ptag}: weight data unreadable")));
                continue;
            }
            stats.checked_prims += 1;
            stats.max_influences = stats.max_influences.max(local_max);
            if over > 0 {
                out.push((
                    "W_OVER_INFLUENCE",
                    format!(
                        "{ptag}: {over} {} exceed {MAX_INFLUENCES} influences \
                         (worst: vertex {over_v} with {over_k})",
                        plural(over, "vertex", "vertices"),
                    ),
                ));
            }
            if bad > 0 {
                out.push((
                    "W_UNNORMALIZED",
                    format!(
                        "{ptag}: {bad} {} with weights not summing to 1.0 \
                         (worst: vertex {bad_v} sum={bad_sum:.3})",
                        plural(bad, "vertex", "vertices"),
                    ),
                ));
            }
        }
    }
    (out, stats)
}

/// Node indices carrying nonzero weight for one mesh, resolved
/// through the skin bound to it. Powers the piece check (a cape may
/// only use bones the body uses). None when the mesh has no skin
/// binding, no skinned prim, or unreadable weight data — the weight
/// layer owns those verdicts, so this only answers "which bones".
/// Validation mirrors `check_weights` deliberately: sharing it would
/// couple the FAIL-level weight verdicts to this advisory scan.
pub(crate) fn used_nodes(json: &Value, bin: &[u8], mi: usize) -> Option<BTreeSet<usize>> {
    let mesh = arr(json, "meshes").get(mi)?;
    let si = arr(json, "nodes").iter().find_map(|n| {
        if n.get("mesh").and_then(as_idx) == Some(mi) {
            n.get("skin").and_then(as_idx)
        } else {
            None
        }
    })?;
    let skin = arr(json, "skins").get(si)?;
    let joints = arr(skin, "joints");
    let accs = arr(json, "accessors");
    let mut used: BTreeSet<usize> = BTreeSet::new();
    let mut any = false;
    for prim in arr(mesh, "primitives") {
        let attrs = prim.get("attributes");
        let acc_idx = |key: &str| attrs.and_then(|a| a.get(key)).and_then(as_idx);
        let (Some(j0), Some(w0)) = (acc_idx("JOINTS_0"), acc_idx("WEIGHTS_0")) else {
            continue;
        };
        let mut sets = vec![(j0, w0)];
        for n in 1.. {
            let jk = format!("JOINTS_{n}");
            let wk = format!("WEIGHTS_{n}");
            match (acc_idx(&jk), acc_idx(&wk)) {
                (None, None) => break,
                (Some(j), Some(w)) => sets.push((j, w)),
                _ => return None,
            }
        }
        if sets.iter().any(|(j, w)| {
            [*j, *w]
                .iter()
                .any(|ai| accs.get(*ai).and_then(|a| a.get("sparse")).is_some())
        }) {
            return None;
        }
        let mut layouts: Vec<(Layout, Layout)> = Vec::new();
        for (j, w) in &sets {
            let (Ok(jl), Ok(wl)) = (
                layout_of(json, bin.len(), *j, "JOINTS"),
                layout_of(json, bin.len(), *w, "WEIGHTS"),
            ) else {
                return None;
            };
            if !joints_ok(&jl) {
                return None;
            }
            let w_ok = weights_ok(&wl);
            if !w_ok {
                return None;
            }
            layouts.push((jl, wl));
        }
        let count = layouts[0].1.count;
        if layouts
            .iter()
            .any(|(j, w)| j.count != count || w.count != count)
        {
            return None;
        }
        for v in 0..count {
            for (jl, wl) in &layouts {
                for k in 0..4 {
                    let (Some(j), Some(x)) = (uint_at(bin, jl, v, k), weight_at(bin, wl, v, k))
                    else {
                        return None;
                    };
                    if x != 0.0 {
                        if let Some(n) = usize::try_from(j)
                            .ok()
                            .and_then(|s| joints.get(s))
                            .and_then(as_idx)
                        {
                            used.insert(n);
                        }
                    }
                }
            }
        }
        any = true;
    }
    any.then_some(used)
}
