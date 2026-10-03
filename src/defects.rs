// SPDX-License-Identifier: MIT
//! v3: objective structural-defect checks (X_* layer).
//!
//! Everything here is provably broken with zero false positives:
//! byte ranges outside their buffer, NaN/Inf animation data, joint
//! indices that reference nothing, geometry that draws nothing,
//! morph counts that disagree, samplers with no keyframes. No
//! thresholds, no budgets, no opinions. Anything unresolvable
//! (dangling references, unknown component types, sparse storage)
//! is skipped silently: this layer reports only what it can prove.

use crate::util::{acc_count, arr, as_idx, as_usize, mesh_tag};
use crate::weights;
use serde_json::Value;

/// Every accessor's dense byte range must sit inside its
/// bufferView, the declared buffer length, and the actual BIN chunk.
fn check_bounds(json: &Value, bin: &[u8], out: &mut Vec<(&'static str, String)>) {
    let accs = arr(json, "accessors");
    let views = arr(json, "bufferViews");
    let bufs = arr(json, "buffers");
    for (ai, acc) in accs.iter().enumerate() {
        // No bufferView: zero-initialized (sparse) data, no range to check.
        let bvi = match acc.get("bufferView").and_then(as_idx) {
            Some(i) => i,
            None => continue,
        };
        let bv = match views.get(bvi) {
            Some(v) => v,
            None => continue,
        };
        let comp = acc
            .get("componentType")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0);
        let csz = match weights::comp_size(comp) {
            Some(s) => s,
            None => continue,
        };
        let ncomp = match acc.get("type").and_then(Value::as_str).unwrap_or("") {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            _ => continue,
        };
        let elem = csz * ncomp;
        let stride = bv
            .get("byteStride")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(elem);
        if stride < elem {
            out.push((
                "X_ACCESSOR_BOUNDS",
                format!("accessor {ai}: byteStride {stride} below element size {elem}"),
            ));
            continue;
        }
        let count = as_usize(acc, "count").unwrap_or(0);
        let view_off = bv
            .get("byteOffset")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0);
        let view_len = match as_usize(bv, "byteLength") {
            Some(l) => l,
            None => continue,
        };
        let acc_off = acc
            .get("byteOffset")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0);
        let (Some(view_end), Some(base)) = (
            view_off.checked_add(view_len),
            view_off.checked_add(acc_off),
        ) else {
            out.push((
                "X_ACCESSOR_BOUNDS",
                format!("accessor {ai}: bufferView {bvi} offsets overflow"),
            ));
            continue;
        };
        let extent = if count == 0 {
            base
        } else {
            match count
                .saturating_sub(1)
                .checked_mul(stride)
                .and_then(|span| base.checked_add(span))
                .and_then(|end| end.checked_add(elem))
            {
                Some(e) => e,
                None => {
                    out.push((
                        "X_ACCESSOR_BOUNDS",
                        format!("accessor {ai}: byte range overflows"),
                    ));
                    continue;
                }
            }
        };
        if extent > view_end {
            out.push((
                "X_ACCESSOR_BOUNDS",
                format!(
                    "accessor {ai}: byte range {base}..{extent} \
                     exceeds bufferView {bvi} ({view_off}..{view_end})"
                ),
            ));
            continue;
        }
        let buf_idx = bv
            .get("buffer")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0);
        if let Some(blen) = bufs.get(buf_idx).and_then(|b| as_usize(b, "byteLength")) {
            if extent > blen {
                out.push((
                    "X_ACCESSOR_BOUNDS",
                    format!(
                        "accessor {ai}: byte range {base}..{extent} \
                         exceeds buffer {buf_idx} byteLength {blen}"
                    ),
                ));
                continue;
            }
        }
        if buf_idx == 0 && extent > bin.len() {
            out.push((
                "X_ACCESSOR_BOUNDS",
                format!(
                    "accessor {ai}: byte range {base}..{extent} \
                     exceeds BIN chunk ({})",
                    bin.len()
                ),
            ));
        }
    }
}

/// Sampler inputs must hold at least one keyframe; float sampler
/// outputs must be finite (NaN/Inf poisons every interpolated frame).
fn check_anims(json: &Value, bin: &[u8], out: &mut Vec<(&'static str, String)>) {
    let accs = arr(json, "accessors");
    let mut scanned: Vec<usize> = Vec::new();
    for (ci, clip) in arr(json, "animations").iter().enumerate() {
        for (si, smp) in arr(clip, "samplers").iter().enumerate() {
            let stag = format!("clip {ci} sampler {si}");
            if let Some(ii) = smp.get("input").and_then(as_idx) {
                if let Some(acc) = accs.get(ii) {
                    let count = as_usize(acc, "count").unwrap_or(0);
                    if count < 1 {
                        out.push((
                            "X_ANIM_KEYS",
                            format!(
                                "{stag}: input accessor {ii} \
                                 has {count} keyframes (< 1)"
                            ),
                        ));
                    }
                }
            }
            let oi = match smp.get("output").and_then(as_idx) {
                Some(o) if !scanned.contains(&o) => o,
                _ => continue,
            };
            scanned.push(oi);
            let acc = match accs.get(oi) {
                Some(a) => a,
                None => continue,
            };
            if acc.get("sparse").is_some() {
                continue;
            }
            if acc.get("componentType").and_then(Value::as_u64) != Some(5126) {
                continue;
            }
            let lay = match weights::layout_of(json, bin.len(), oi, "OUTPUT") {
                Ok(l) => l,
                Err(_) => continue,
            };
            let mut bad = 0usize;
            let mut first = 0usize;
            let mut total = 0usize;
            let mut readable = true;
            for v in 0..lay.count {
                for k in 0..lay.ncomp {
                    match weights::f32_at(bin, &lay, v, k) {
                        Some(x) => {
                            if !x.is_finite() {
                                if bad == 0 {
                                    first = total;
                                }
                                bad += 1;
                            }
                        }
                        None => {
                            readable = false;
                            break;
                        }
                    }
                    total += 1;
                }
                if !readable {
                    break;
                }
            }
            if readable && bad > 0 {
                out.push((
                    "X_ANIM_NAN",
                    format!(
                        "{stag}: output accessor {oi} has {bad} non-finite float{} \
                         (first at component {first})",
                        if bad == 1 { "" } else { "s" },
                    ),
                ));
            }
        }
    }
}

/// Every JOINTS_n index must address a joint of the skin bound to
/// the mesh (< skin.joints length).
fn check_joint_range(json: &Value, bin: &[u8], out: &mut Vec<(&'static str, String)>) {
    let meshes = arr(json, "meshes");
    let skins = arr(json, "skins");
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for n in arr(json, "nodes") {
        if let (Some(mi), Some(si)) = (
            n.get("mesh").and_then(as_idx),
            n.get("skin").and_then(as_idx),
        ) {
            if meshes.get(mi).is_some() && skins.get(si).is_some() && !pairs.contains(&(mi, si)) {
                pairs.push((mi, si));
            }
        }
    }
    for (mi, si) in pairs {
        let jlen = arr(&skins[si], "joints").len();
        let mtag = mesh_tag(meshes, mi);
        for (pi, prim) in arr(&meshes[mi], "primitives").iter().enumerate() {
            let attrs = prim.get("attributes");
            for n in 0.. {
                let jk = format!("JOINTS_{n}");
                let ji = match attrs.and_then(|a| a.get(&jk)).and_then(as_idx) {
                    Some(i) => i,
                    None => break,
                };
                if arr(json, "accessors")
                    .get(ji)
                    .and_then(|a| a.get("sparse"))
                    .is_some()
                {
                    continue;
                }
                let lay = match weights::layout_of(json, bin.len(), ji, &jk) {
                    Ok(l) => l,
                    Err(_) => continue,
                };
                if lay.ncomp != 4 || (lay.comp != 5121 && lay.comp != 5123) {
                    continue;
                }
                let mut bad = 0usize;
                let mut bad_v = 0usize;
                let mut bad_j = 0u32;
                let mut readable = true;
                for v in 0..lay.count {
                    for k in 0..4 {
                        match weights::uint_at(bin, &lay, v, k) {
                            Some(j) => {
                                if usize::try_from(j).is_ok_and(|idx| idx >= jlen) {
                                    bad += 1;
                                    if bad == 1 {
                                        bad_v = v;
                                        bad_j = j;
                                    }
                                }
                            }
                            None => {
                                readable = false;
                                break;
                            }
                        }
                    }
                    if !readable {
                        break;
                    }
                }
                if readable && bad > 0 {
                    out.push((
                        "X_JOINT_RANGE",
                        format!(
                            "{mtag} prim {pi} {jk} accessor {ji}: {bad} {} \
                             nothing (skin {si} has {jlen} joints; \
                             first: vertex {bad_v} index {bad_j})",
                            if bad == 1 {
                                "index references"
                            } else {
                                "indices reference"
                            },
                        ),
                    ));
                }
            }
        }
    }
}

/// Geometry that draws nothing: meshes with no primitives or zero
/// vertices, and primitives with zero vertices and zero indices.
fn check_empty(json: &Value, out: &mut Vec<(&'static str, String)>) {
    let meshes = arr(json, "meshes");
    for (mi, mesh) in meshes.iter().enumerate() {
        let mtag = mesh_tag(meshes, mi);
        let prims = arr(mesh, "primitives");
        if prims.is_empty() {
            out.push(("X_EMPTY_MESH", format!("{mtag}: mesh has no primitives")));
            continue;
        }
        let mut total = 0usize;
        let mut all_known = true;
        for prim in prims {
            match prim
                .get("attributes")
                .and_then(|a| a.get("POSITION"))
                .and_then(as_idx)
                .and_then(|ai| acc_count(json, ai))
            {
                Some(c) => total = total.saturating_add(c),
                None => all_known = false,
            }
        }
        if all_known && total == 0 {
            out.push((
                "X_EMPTY_MESH",
                format!(
                    "{mtag}: mesh has zero vertices across {} primitive{}",
                    prims.len(),
                    if prims.len() == 1 { "" } else { "s" },
                ),
            ));
        }
        for (pi, prim) in prims.iter().enumerate() {
            let verts = prim
                .get("attributes")
                .and_then(|a| a.get("POSITION"))
                .and_then(as_idx)
                .and_then(|ai| acc_count(json, ai));
            if verts != Some(0) {
                continue;
            }
            match prim.get("indices").and_then(as_idx) {
                None => out.push((
                    "X_EMPTY_PRIM",
                    format!("{mtag} prim {pi}: zero vertices (draws nothing)"),
                )),
                Some(ii) => {
                    if acc_count(json, ii) == Some(0) {
                        out.push((
                            "X_EMPTY_PRIM",
                            format!(
                                "{mtag} prim {pi}: zero vertices \
                                 and zero indices (draws nothing)"
                            ),
                        ));
                    }
                }
            }
        }
    }
}

/// Morph-target counts must agree: mesh.weights length matches each
/// primitive's targets length, and every target accessor holds one
/// value per POSITION vertex.
fn check_morph(json: &Value, out: &mut Vec<(&'static str, String)>) {
    let meshes = arr(json, "meshes");
    for (mi, mesh) in meshes.iter().enumerate() {
        let mtag = mesh_tag(meshes, mi);
        let wlen = mesh.get("weights").and_then(Value::as_array).map(Vec::len);
        for (pi, prim) in arr(mesh, "primitives").iter().enumerate() {
            let ptag = format!("{mtag} prim {pi}");
            let targets = prim.get("targets").and_then(Value::as_array);
            if let Some(w) = wlen {
                let t = targets.map_or(0, Vec::len);
                if t != w {
                    out.push((
                        "X_MORPH_COUNT",
                        format!(
                            "{ptag}: {t} morph target{} \
                             but mesh.weights has {w}",
                            if t == 1 { "" } else { "s" },
                        ),
                    ));
                }
            }
            let (Some(tgts), Some(pcount)) = (
                targets,
                prim.get("attributes")
                    .and_then(|a| a.get("POSITION"))
                    .and_then(as_idx)
                    .and_then(|ai| acc_count(json, ai)),
            ) else {
                continue;
            };
            let mut mism = 0usize;
            let mut first = String::new();
            for (ti, tgt) in tgts.iter().enumerate() {
                let obj = match tgt.as_object() {
                    Some(o) => o,
                    None => continue,
                };
                for (attr, v) in obj {
                    let c = match as_idx(v).and_then(|ai| acc_count(json, ai)) {
                        Some(c) => c,
                        None => continue,
                    };
                    if c != pcount {
                        mism += 1;
                        if first.is_empty() {
                            first = format!("target {ti} '{attr}' count {c}");
                        }
                    }
                }
            }
            if mism > 0 {
                out.push((
                    "X_MORPH_COUNT",
                    format!(
                        "{ptag}: {mism} morph accessor{} with count \
                         != POSITION count {pcount} (first: {first})",
                        if mism == 1 { "" } else { "s" },
                    ),
                ));
            }
        }
    }
}

pub fn check_defects(json: &Value, bin: &[u8]) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    check_bounds(json, bin, &mut out);
    check_anims(json, bin, &mut out);
    check_joint_range(json, bin, &mut out);
    check_empty(json, &mut out);
    check_morph(json, &mut out);
    out
}
