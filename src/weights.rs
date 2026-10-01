// SPDX-License-Identifier: MIT
//! v2: skin-weight stats via BIN accessor parsing.
//!
//! For every mesh bound to a skin, reads JOINTS_n/WEIGHTS_n accessors
//! out of the GLB BIN chunk and reports over-limit influence counts
//! and unnormalized weights. Anything unreadable (bad references,
//! out-of-bounds reads, sparse storage) is an explicit diagnostic,
//! never a silent skip or a panic.

use serde_json::Value;

/// Game-rig limit: at most 4 nonzero influences per vertex.
pub const MAX_INFLUENCES: usize = 4;
/// Weight sums must land within this distance of 1.0.
pub const WEIGHT_TOL: f64 = 1e-3;

pub struct SkinStats {
    pub max_influences: usize,
    pub checked_prims: usize,
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

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 {
        one
    } else {
        many
    }
}

pub(crate) struct Layout {
    pub(crate) base: usize,
    pub(crate) stride: usize,
    pub(crate) count: usize,
    pub(crate) comp: u32,
    pub(crate) ncomp: usize,
    pub(crate) normalized: bool,
}

pub(crate) fn comp_size(comp: u32) -> Option<usize> {
    match comp {
        5120 | 5121 => Some(1),
        5122 | 5123 => Some(2),
        5125 | 5126 => Some(4),
        _ => None,
    }
}

/// Resolve an accessor to its BIN layout, bounds-checked. `what` names
/// the attribute for error detail (e.g. "WEIGHTS_0").
pub(crate) fn layout_of(
    json: &Value,
    bin_len: usize,
    ai: usize,
    what: &str,
) -> Result<Layout, String> {
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
    if bv.get("buffer").and_then(Value::as_u64).unwrap_or(0) != 0 {
        return Err(format!("{what} accessor {ai} uses a non-GLB buffer"));
    }
    let comp = acc
        .get("componentType")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0);
    let csz =
        comp_size(comp).ok_or_else(|| format!("{what} accessor {ai} has componentType {comp}"))?;
    let ncomp = match acc.get("type").and_then(Value::as_str).unwrap_or("") {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        other => return Err(format!("{what} accessor {ai} has type {other:?}")),
    };
    let elem = csz * ncomp;
    let stride = bv
        .get("byteStride")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(elem);
    if stride < elem {
        return Err(format!(
            "{what} accessor {ai} has byteStride {stride} below element size {elem}"
        ));
    }
    let count = acc
        .get("count")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0);
    let off = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0)
    };
    let base = off(bv, "byteOffset")
        .checked_add(off(acc, "byteOffset"))
        .ok_or_else(|| format!("{what} accessor {ai} has overflowing offsets"))?;
    let end = if count == 0 {
        base
    } else {
        base.checked_add(
            count
                .saturating_sub(1)
                .checked_mul(stride)
                .ok_or_else(|| format!("{what} accessor {ai} extent overflows"))?,
        )
        .and_then(|v| v.checked_add(elem))
        .ok_or_else(|| format!("{what} accessor {ai} extent overflows"))?
    };
    if base > bin_len || end > bin_len {
        return Err(format!(
            "{what} accessor {ai} reads past the BIN chunk ({end} > {bin_len})"
        ));
    }
    Ok(Layout {
        base,
        stride,
        count,
        comp,
        ncomp,
        normalized: acc
            .get("normalized")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// Read one weight value with normalized-int conversion. Fully
/// bounds-checked; None means the layout lied (cannot happen after
/// `layout_of`, but corrupt input must never panic).
fn weight_at(bin: &[u8], lay: &Layout, v: usize, k: usize) -> Option<f64> {
    let csz = comp_size(lay.comp)?;
    let o = lay
        .base
        .checked_add(v.checked_mul(lay.stride)?)?
        .checked_add(k.checked_mul(csz)?)?;
    match lay.comp {
        5126 => Some(f32::from_le_bytes(bin.get(o..o + 4)?.try_into().ok()?) as f64),
        5121 => Some(f64::from(*bin.get(o)?) / 255.0),
        5123 => Some(f64::from(u16::from_le_bytes(bin.get(o..o + 2)?.try_into().ok()?)) / 65535.0),
        _ => None,
    }
}

/// Raw f32 read (FLOAT accessors only). Bounds-checked; None on
/// any mismatch, so corrupt input can never panic.
pub(crate) fn f32_at(bin: &[u8], lay: &Layout, v: usize, k: usize) -> Option<f32> {
    if lay.comp != 5126 {
        return None;
    }
    let o = lay
        .base
        .checked_add(v.checked_mul(lay.stride)?)?
        .checked_add(k.checked_mul(4)?)?;
    Some(f32::from_le_bytes(bin.get(o..o + 4)?.try_into().ok()?))
}

/// Raw joint-index read (UNSIGNED_BYTE/SHORT accessors only).
/// Bounds-checked; None on any mismatch.
pub(crate) fn uint_at(bin: &[u8], lay: &Layout, v: usize, k: usize) -> Option<u32> {
    let csz = comp_size(lay.comp)?;
    let o = lay
        .base
        .checked_add(v.checked_mul(lay.stride)?)?
        .checked_add(k.checked_mul(csz)?)?;
    match lay.comp {
        5121 => Some(u32::from(*bin.get(o)?)),
        5123 => Some(u32::from(u16::from_le_bytes(
            bin.get(o..o + 2)?.try_into().ok()?,
        ))),
        _ => None,
    }
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
                if jl.ncomp != 4 || (jl.comp != 5121 && jl.comp != 5123) {
                    out.push((
                        "W_BAD_ACCESSOR",
                        format!("{ptag}: {jt} accessor {j} must be VEC4 UNSIGNED_BYTE/SHORT"),
                    ));
                    skipped = true;
                    break;
                }
                let w_ok = wl.ncomp == 4
                    && (wl.comp == 5126 || (wl.normalized && (wl.comp == 5121 || wl.comp == 5123)));
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
