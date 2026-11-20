// SPDX-License-Identifier: MIT
//! Reference-compared mesh checks (M_FOLD against reference surface,
//! M_SILHOUETTE_DRIFT on talk-camera head/chin/jaw/ears/tips contours).
//!
//! Zero external dependencies, deterministic across platforms, runs in milliseconds.

use crate::glb;
use crate::util::{arr, as_idx};
use crate::weights;
use glbkit::accessor::{Component, Layout};
use serde_json::Value;
use std::collections::BTreeMap;

const WELD_SCALE: f64 = 10_000.0;
const MIN_AREA2: f64 = 1e-12;
const TALK_CAM_RES: usize = 512;
const FOLD_ANGLE_DEG: f64 = 35.0;
const SILHOUETTE_DRIFT_PX: f64 = 1.5;

#[derive(Clone, Copy)]
struct Tri {
    p: [[f64; 3]; 3],
    n: [f64; 3],
    center: [f64; 3],
}

#[inline]
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
fn len(v: [f64; 3]) -> f64 {
    dot(v, v).sqrt()
}

#[inline]
fn unit(v: [f64; 3]) -> [f64; 3] {
    let l = len(v);
    if l > 1e-12 {
        [v[0] / l, v[1] / l, v[2] / l]
    } else {
        [0.0, 1.0, 0.0]
    }
}

/// Column-major 4x4 matrix multiplication `a * b`.
fn mat4_mul(a: &[f64; 16], b: &[f64; 16]) -> [f64; 16] {
    let mut out = [0.0f64; 16];
    for c in 0..4 {
        for r in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k * 4 + r] * b[c * 4 + k];
            }
            out[c * 4 + r] = sum;
        }
    }
    out
}

fn mat4_identity() -> [f64; 16] {
    [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ]
}

fn node_local_matrix(n: &Value) -> [f64; 16] {
    if let Some(m) = n.get("matrix").and_then(Value::as_array) {
        if m.len() == 16 {
            let mut out = [0.0f64; 16];
            for (i, v) in m.iter().enumerate() {
                out[i] = v.as_f64().unwrap_or(0.0);
            }
            return out;
        }
    }
    let t = n
        .get("translation")
        .and_then(Value::as_array)
        .filter(|a| a.len() == 3)
        .map(|a| {
            [
                a[0].as_f64().unwrap_or(0.0),
                a[1].as_f64().unwrap_or(0.0),
                a[2].as_f64().unwrap_or(0.0),
            ]
        })
        .unwrap_or([0.0, 0.0, 0.0]);
    let s = n
        .get("scale")
        .and_then(Value::as_array)
        .filter(|a| a.len() == 3)
        .map(|a| {
            [
                a[0].as_f64().unwrap_or(1.0),
                a[1].as_f64().unwrap_or(1.0),
                a[2].as_f64().unwrap_or(1.0),
            ]
        })
        .unwrap_or([1.0, 1.0, 1.0]);
    let q = n
        .get("rotation")
        .and_then(Value::as_array)
        .filter(|a| a.len() == 4)
        .map(|a| {
            [
                a[0].as_f64().unwrap_or(0.0),
                a[1].as_f64().unwrap_or(0.0),
                a[2].as_f64().unwrap_or(0.0),
                a[3].as_f64().unwrap_or(1.0),
            ]
        })
        .unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let [qx, qy, qz, qw] = q;
    let xx = qx * qx;
    let yy = qy * qy;
    let zz = qz * qz;
    let xy = qx * qy;
    let xz = qx * qz;
    let yz = qy * qz;
    let wx = qw * qx;
    let wy = qw * qy;
    let wz = qw * qz;

    let r00 = 1.0 - 2.0 * (yy + zz);
    let r01 = 2.0 * (xy - wz);
    let r02 = 2.0 * (xz + wy);
    let r10 = 2.0 * (xy + wz);
    let r11 = 1.0 - 2.0 * (xx + zz);
    let r12 = 2.0 * (yz - wx);
    let r20 = 2.0 * (xz - wy);
    let r21 = 2.0 * (yz + wx);
    let r22 = 1.0 - 2.0 * (xx + yy);

    [
        r00 * s[0],
        r10 * s[0],
        r20 * s[0],
        0.0,
        r01 * s[1],
        r11 * s[1],
        r21 * s[1],
        0.0,
        r02 * s[2],
        r12 * s[2],
        r22 * s[2],
        0.0,
        t[0],
        t[1],
        t[2],
        1.0,
    ]
}

fn transform_point(m: &[f64; 16], p: [f64; 3]) -> [f64; 3] {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]
}

fn mat4_det3(m: &[f64; 16]) -> f64 {
    let c0 = [m[0], m[1], m[2]];
    let c1 = [m[4], m[5], m[6]];
    let c2 = [m[8], m[9], m[10]];
    dot(c0, cross(c1, c2))
}

fn is_sparse(json: &Value, ai: usize) -> bool {
    arr(json, "accessors")
        .get(ai)
        .and_then(|a| a.get("sparse"))
        .is_some()
}

fn read_vec3(bin: &[u8], lay: &Layout, idx: usize) -> Option<[f64; 3]> {
    let x = f64::from(weights::f32_at(bin, lay, idx, 0)?);
    let y = f64::from(weights::f32_at(bin, lay, idx, 1)?);
    let z = f64::from(weights::f32_at(bin, lay, idx, 2)?);
    if x.is_finite() && y.is_finite() && z.is_finite() {
        Some([x, y, z])
    } else {
        None
    }
}

fn extract_world_tris(json: &Value, bin: &[u8]) -> Vec<Tri> {
    let nodes = arr(json, "nodes");
    let meshes = arr(json, "meshes");
    let mut parent: Vec<Option<usize>> = vec![None; nodes.len()];
    for (pi, n) in nodes.iter().enumerate() {
        for c in arr(n, "children") {
            if let Some(ci) = as_idx(c) {
                if ci < nodes.len() {
                    parent[ci] = Some(pi);
                }
            }
        }
    }
    // Collect (mesh_idx, world_matrix) instances. If no node references any mesh,
    // fall back to identity for every mesh so minimal unit fixtures work seamlessly.
    let mut instances: Vec<(usize, [f64; 16])> = Vec::new();
    for (ni, n) in nodes.iter().enumerate() {
        let Some(mi) = n.get("mesh").and_then(as_idx) else {
            continue;
        };
        if mi >= meshes.len() {
            continue;
        }
        let mut chain = Vec::new();
        let mut cur = Some(ni);
        let mut hops = 0usize;
        while let Some(idx) = cur {
            if hops > nodes.len() {
                break;
            }
            chain.push(idx);
            cur = parent[idx];
            hops += 1;
        }
        let mut wm = mat4_identity();
        for &idx in chain.iter().rev() {
            wm = mat4_mul(&wm, &node_local_matrix(&nodes[idx]));
        }
        instances.push((mi, wm));
    }
    if instances.is_empty() {
        for mi in 0..meshes.len() {
            instances.push((mi, mat4_identity()));
        }
    }

    let mut tris = Vec::new();
    for (mi, wm) in instances {
        let flip_winding = mat4_det3(&wm) < 0.0;
        let mesh = &meshes[mi];
        for prim in arr(mesh, "primitives") {
            let mode = prim.get("mode").and_then(Value::as_u64).unwrap_or(4);
            if mode != 4 && mode != 5 {
                continue;
            }
            let Some(attrs) = prim.get("attributes") else {
                continue;
            };
            let Some(pos_i) = attrs.get("POSITION").and_then(as_idx) else {
                continue;
            };
            if is_sparse(json, pos_i) {
                continue;
            }
            let Ok(pos) = weights::layout_of(json, bin.len(), pos_i, "POSITION") else {
                continue;
            };
            if pos.component != Component::F32 || pos.width != 3 || pos.count < 3 {
                continue;
            }
            let idx_lay: Option<Layout> = match prim.get("indices").and_then(as_idx) {
                None => None,
                Some(ii) => {
                    if is_sparse(json, ii) {
                        continue;
                    }
                    let Ok(lay) = weights::layout_of(json, bin.len(), ii, "indices") else {
                        continue;
                    };
                    if lay.width != 1
                        || !matches!(
                            lay.component,
                            Component::U8 | Component::U16 | Component::U32
                        )
                    {
                        continue;
                    }
                    Some(lay)
                }
            };
            let n_idx = idx_lay.map_or(pos.count, |l| l.count);
            if n_idx < 3 {
                continue;
            }
            let read_idx = |k: usize| -> Option<usize> {
                match &idx_lay {
                    Some(lay) => usize::try_from(lay.read_uint(bin, k, 0)?).ok(),
                    None => Some(k),
                }
            };
            let mut push_tri = |a: usize, b: usize, c: usize| {
                let (Some(pa), Some(pb), Some(pc)) = (
                    read_vec3(bin, &pos, a),
                    read_vec3(bin, &pos, b),
                    read_vec3(bin, &pos, c),
                ) else {
                    return;
                };
                let wa = transform_point(&wm, pa);
                let (wb, wc) = if flip_winding {
                    (transform_point(&wm, pc), transform_point(&wm, pb))
                } else {
                    (transform_point(&wm, pb), transform_point(&wm, pc))
                };
                let face = cross(sub(wb, wa), sub(wc, wa));
                let fl = len(face);
                if !fl.is_finite() || fl <= MIN_AREA2 {
                    return;
                }
                let n = [face[0] / fl, face[1] / fl, face[2] / fl];
                let center = [
                    (wa[0] + wb[0] + wc[0]) / 3.0,
                    (wa[1] + wb[1] + wc[1]) / 3.0,
                    (wa[2] + wb[2] + wc[2]) / 3.0,
                ];
                tris.push(Tri {
                    p: [wa, wb, wc],
                    n,
                    center,
                });
            };
            if mode == 4 {
                let mut k = 0usize;
                while k + 2 < n_idx {
                    if let (Some(a), Some(b), Some(c)) =
                        (read_idx(k), read_idx(k + 1), read_idx(k + 2))
                    {
                        if a < pos.count && b < pos.count && c < pos.count {
                            push_tri(a, b, c);
                        }
                    }
                    k += 3;
                }
            } else {
                for k in 0..n_idx - 2 {
                    if let (Some(a), Some(b), Some(c)) =
                        (read_idx(k), read_idx(k + 1), read_idx(k + 2))
                    {
                        if a < pos.count && b < pos.count && c < pos.count {
                            if k % 2 == 0 {
                                push_tri(a, b, c);
                            } else {
                                push_tri(a, c, b);
                            }
                        }
                    }
                }
            }
        }
    }
    tris
}

fn bbox(tris: &[Tri]) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for t in tris {
        for p in &t.p {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    (lo, hi)
}

/// Normalize candidate to match reference bbox ONLY if scale/origin/90-deg yaw clearly differ
/// (e.g., raw unscaled/rotated Tripo reference vs 1.75m canonicalized candidate).
fn align_if_different_frame(cand: &mut [Tri], ref_tris: &[Tri]) {
    let (rlo, rhi) = bbox(ref_tris);
    let (clo, chi) = bbox(cand);
    let rh = (rhi[1] - rlo[1]).max(1e-6);
    let ch = (chi[1] - clo[1]).max(1e-6);
    let rcx = 0.5 * (rlo[0] + rhi[0]);
    let rcz = 0.5 * (rlo[2] + rhi[2]);
    let ccx = 0.5 * (clo[0] + chi[0]);
    let ccz = 0.5 * (clo[2] + chi[2]);

    // Check if a 90/180/270-deg yaw around +Y dramatically improves silhouette IoU
    // (handles comparing a canonicalized +Z-facing character against a raw Tripo source).
    let span = 1.15 * (rhi[0] - rlo[0]).max(rh).max(rhi[2] - rlo[2]).max(1e-6);
    let r_center = [rcx, rlo[1] + 0.5 * rh, rcz];
    let r_sil_f = rasterize_ortho_silhouette(ref_tris, r_center, span, 0.0, 96);
    let r_sil_s = rasterize_ortho_silhouette(ref_tris, r_center, span, -90.0, 96);

    let iou = |a: &[bool], b: &[bool]| -> f64 {
        let mut inter = 0usize;
        let mut uni = 0usize;
        for (&x, &y) in a.iter().zip(b.iter()) {
            if x && y {
                inter += 1;
            }
            if x || y {
                uni += 1;
            }
        }
        if uni == 0 {
            0.0
        } else {
            (inter as f64) / (uni as f64)
        }
    };

    let scale_diff = (ch - rh).abs() / rh;
    let shift = ((ccx - rcx).powi(2) + (clo[1] - rlo[1]).powi(2) + (ccz - rcz).powi(2)).sqrt();
    let need_scale_shift = scale_diff > 0.05 || shift > 0.08 * rh;
    let s = if need_scale_shift { rh / ch } else { 1.0 };

    let apply_xform = |src: &[Tri], rot_q: usize, do_scale_shift: bool| -> Vec<Tri> {
        let mut out = Vec::with_capacity(src.len());
        for t in src {
            let mut np = [[0.0; 3]; 3];
            for (k, &p) in t.p.iter().enumerate() {
                let mut x = p[0] - ccx;
                let mut y = p[1] - clo[1];
                let mut z = p[2] - ccz;
                if do_scale_shift {
                    x *= s;
                    y *= s;
                    z *= s;
                }
                let (rx, rz) = match rot_q {
                    1 => (z, -x),
                    2 => (-x, -z),
                    3 => (-z, x),
                    _ => (x, z),
                };
                np[k] = if do_scale_shift || rot_q != 0 {
                    [rx + rcx, y + rlo[1], rz + rcz]
                } else {
                    p
                };
            }
            let face = cross(sub(np[1], np[0]), sub(np[2], np[0]));
            out.push(Tri {
                p: np,
                n: unit(face),
                center: [
                    (np[0][0] + np[1][0] + np[2][0]) / 3.0,
                    (np[0][1] + np[1][1] + np[2][1]) / 3.0,
                    (np[0][2] + np[1][2] + np[2][2]) / 3.0,
                ],
            });
        }
        out
    };

    let base = if need_scale_shift {
        apply_xform(cand, 0, true)
    } else {
        cand.to_vec()
    };
    let base_iou = 0.5
        * (iou(
            &r_sil_f,
            &rasterize_ortho_silhouette(&base, r_center, span, 0.0, 96),
        ) + iou(
            &r_sil_s,
            &rasterize_ortho_silhouette(&base, r_center, span, -90.0, 96),
        ));

    let mut best_q = 0usize;
    let mut best_iou = base_iou;
    if base_iou < 0.85 {
        for q in 1..4 {
            let trial = apply_xform(cand, q, need_scale_shift);
            let tiou = 0.5
                * (iou(
                    &r_sil_f,
                    &rasterize_ortho_silhouette(&trial, r_center, span, 0.0, 96),
                ) + iou(
                    &r_sil_s,
                    &rasterize_ortho_silhouette(&trial, r_center, span, -90.0, 96),
                ));
            if tiou > best_iou + 0.12 {
                best_iou = tiou;
                best_q = q;
            }
        }
    }
    if best_q != 0 || need_scale_shift {
        let aligned = apply_xform(cand, best_q, need_scale_shift);
        cand.copy_from_slice(&aligned);
    }
}

/// Closest point on triangle `(a, b, c)` to point `p`, returning `(sq_dist, barycentric_weights)`.
fn closest_point_on_tri(p: [f64; 3], a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> (f64, [f64; 3]) {
    let ab = sub(b, a);
    let ac = sub(c, a);
    let ap = sub(p, a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        let d = sub(p, a);
        return (dot(d, d), [1.0, 0.0, 0.0]);
    }
    let bp = sub(p, b);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        let d = sub(p, b);
        return (dot(d, d), [0.0, 1.0, 0.0]);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        let q = add(a, scale(ab, v));
        let d = sub(p, q);
        return (dot(d, d), [1.0 - v, v, 0.0]);
    }
    let cp = sub(p, c);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        let d = sub(p, c);
        return (dot(d, d), [0.0, 0.0, 1.0]);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        let q = add(a, scale(ac, w));
        let d = sub(p, q);
        return (dot(d, d), [1.0 - w, 0.0, w]);
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        let q = add(b, scale(sub(c, b), w));
        let d = sub(p, q);
        return (dot(d, d), [0.0, 1.0 - w, w]);
    }
    let denom = 1.0 / (va + vb + vc).max(1e-30);
    let v = vb * denom;
    let w = vc * denom;
    let u = 1.0 - v - w;
    let q = add(a, add(scale(ab, v), scale(ac, w)));
    let d = sub(p, q);
    (dot(d, d), [u, v, w])
}

/// Compare candidate triangle normals against smoothed reference surface normals.
/// Flags `M_FOLD` when triangles diverge > 35 deg from the reference surface.
fn check_reference_folds(cand: &[Tri], ref_tris: &[Tri], out: &mut Vec<(&'static str, String)>) {
    if cand.is_empty() || ref_tris.is_empty() {
        return;
    }
    let (rlo, rhi) = bbox(ref_tris);
    let span = (rhi[0] - rlo[0])
        .max(rhi[1] - rlo[1])
        .max(rhi[2] - rlo[2])
        .max(1e-6);
    // Compute area-weighted smooth vertex normals on reference (welded by position)
    // so micro-relief on a high-poly sculpt does not create false fold alarms.
    let mut vn_acc: BTreeMap<[i64; 3], [f64; 3]> = BTreeMap::new();
    let wkey = |p: [f64; 3]| -> [i64; 3] {
        [
            (p[0] * WELD_SCALE).round() as i64,
            (p[1] * WELD_SCALE).round() as i64,
            (p[2] * WELD_SCALE).round() as i64,
        ]
    };
    for t in ref_tris {
        let area2 = len(cross(sub(t.p[1], t.p[0]), sub(t.p[2], t.p[0])));
        let wn = scale(t.n, area2);
        for &p in &t.p {
            let entry = vn_acc.entry(wkey(p)).or_insert([0.0; 3]);
            entry[0] += wn[0];
            entry[1] += wn[1];
            entry[2] += wn[2];
        }
    }
    let mut ref_smooth_n: Vec<[[f64; 3]; 3]> = Vec::with_capacity(ref_tris.len());
    for t in ref_tris {
        let mut cn = [[0.0; 3]; 3];
        for (k, &p) in t.p.iter().enumerate() {
            let acc = vn_acc.get(&wkey(p)).copied().unwrap_or(t.n);
            cn[k] = if len(acc) > 1e-12 { unit(acc) } else { t.n };
        }
        ref_smooth_n.push(cn);
    }

    // Spatial hash grid over reference triangle centers (coarse enough to keep lookup fast).
    let cell = (span / 48.0).max(1e-4);
    let mut grid: BTreeMap<[i32; 3], Vec<usize>> = BTreeMap::new();
    let cell_of = |p: [f64; 3]| -> [i32; 3] {
        [
            ((p[0] - rlo[0]) / cell).floor() as i32,
            ((p[1] - rlo[1]) / cell).floor() as i32,
            ((p[2] - rlo[2]) / cell).floor() as i32,
        ]
    };
    // Subsample huge (>250k) reference grids deterministically for speed while retaining coverage.
    let ref_step = (ref_tris.len() / 200_000).max(1);
    for (ti, t) in ref_tris.iter().enumerate().step_by(ref_step) {
        let c = cell_of(t.center);
        grid.entry(c).or_default().push(ti);
    }

    let cos_35 = FOLD_ANGLE_DEG.to_radians().cos();
    let cos_60 = 60.0f64.to_radians().cos();
    let max_match_sq = (0.04 * span).powi(2);

    let cand_step = (cand.len() / 30_000).max(1);
    let mut checked = 0usize;
    let mut folds_35 = 0usize;
    let mut folds_60 = 0usize;
    let mut worst_deg = 0.0f64;

    for t in cand.iter().step_by(cand_step) {
        let cc = cell_of(t.center);
        let mut best_d2 = f64::INFINITY;
        let mut best_hit: Option<(usize, [f64; 3])> = None;
        for rad in 0..=2i32 {
            for dx in -rad..=rad {
                for dy in -rad..=rad {
                    for dz in -rad..=rad {
                        if rad > 0 && dx.abs() < rad && dy.abs() < rad && dz.abs() < rad {
                            continue;
                        }
                        if let Some(list) = grid.get(&[cc[0] + dx, cc[1] + dy, cc[2] + dz]) {
                            for &ri in list {
                                let rt = &ref_tris[ri];
                                let (d2, bary) =
                                    closest_point_on_tri(t.center, rt.p[0], rt.p[1], rt.p[2]);
                                if d2 < best_d2 {
                                    best_d2 = d2;
                                    best_hit = Some((ri, bary));
                                }
                            }
                        }
                    }
                }
            }
            if best_hit.is_some() && best_d2 <= (cell * (rad as f64 + 0.5)).powi(2) {
                break;
            }
        }
        let Some((ri, bary)) = best_hit else {
            continue;
        };
        if best_d2 > max_match_sq {
            continue;
        }
        checked += 1;
        let rsn = &ref_smooth_n[ri];
        let interp = unit([
            rsn[0][0] * bary[0] + rsn[1][0] * bary[1] + rsn[2][0] * bary[2],
            rsn[0][1] * bary[0] + rsn[1][1] * bary[1] + rsn[2][1] * bary[2],
            rsn[0][2] * bary[0] + rsn[1][2] * bary[1] + rsn[2][2] * bary[2],
        ]);
        // Compare against both smooth interpolated normal and raw face normal;
        // take the closer one so sharp intentional creases on the reference don't false-alarm.
        let d_smooth = dot(t.n, interp).clamp(-1.0, 1.0);
        let d_face = dot(t.n, ref_tris[ri].n).clamp(-1.0, 1.0);
        let d_best = d_smooth.max(d_face);
        if d_best < cos_35 {
            folds_35 += 1;
            let deg = d_best.acos().to_degrees();
            if deg > worst_deg {
                worst_deg = deg;
            }
            if d_best < cos_60 {
                folds_60 += 1;
            }
        }
    }

    if checked == 0 {
        return;
    }
    let pct35 = (folds_35 as f64) * 100.0 / (checked as f64);
    let bad_35 = folds_35 >= 4 && pct35 >= 1.5;
    let bad_60 = folds_60 >= 4 && (folds_60 as f64) * 100.0 / (checked as f64) >= 0.4;
    if bad_35 || bad_60 {
        out.push((
            "M_FOLD",
            format!(
                "reference comparison: {folds_35} of {checked} sampled triangles ({pct35:.1}%) \
                 diverge > {FOLD_ANGLE_DEG:.0} deg from reference surface \
                 ({folds_60} > 60 deg, worst {worst_deg:.1} deg)"
            ),
        ));
    }
}

/// 1D squared Euclidean distance transform (Felzenszwalb-Huttenlocher, O(n)).
fn edt_1d(f: &[f64], out: &mut [f64], v: &mut [usize], z: &mut [f64]) {
    let n = f.len();
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    for q in 1..n {
        let mut s = ((f[q] + (q * q) as f64) - (f[v[k]] + (v[k] * v[k]) as f64))
            / (2.0 * (q as f64 - v[k] as f64));
        while s <= z[k] {
            if k == 0 {
                break;
            }
            k -= 1;
            s = ((f[q] + (q * q) as f64) - (f[v[k]] + (v[k] * v[k]) as f64))
                / (2.0 * (q as f64 - v[k] as f64));
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f64::INFINITY;
    }
    k = 0;
    for (q, dst) in out.iter_mut().enumerate().take(n) {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let dq = q as f64 - v[k] as f64;
        *dst = dq * dq + f[v[k]];
    }
}

/// 2D exact Euclidean distance transform from `true` pixels in `mask` (w x h).
fn distance_transform_2d(mask: &[bool], w: usize, h: usize) -> Vec<f64> {
    const INF: f64 = 1e12;
    let mut tmp = vec![INF; w * h];
    let mut f = vec![0.0f64; w.max(h)];
    let mut d = vec![0.0f64; w.max(h)];
    let mut v = vec![0usize; w.max(h)];
    let mut z = vec![0.0f64; w.max(h) + 1];

    // Columns
    for x in 0..w {
        for y in 0..h {
            f[y] = if mask[y * w + x] { 0.0 } else { INF };
        }
        edt_1d(&f[..h], &mut d[..h], &mut v[..h], &mut z[..=h]);
        for y in 0..h {
            tmp[y * w + x] = d[y];
        }
    }
    // Rows
    let mut out = vec![0.0f64; w * h];
    for y in 0..h {
        let row = &tmp[y * w..(y + 1) * w];
        edt_1d(row, &mut d[..w], &mut v[..w], &mut z[..=w]);
        for x in 0..w {
            out[y * w + x] = d[x].sqrt();
        }
    }
    out
}

/// Rasterize filled silhouette of `tris` onto a `res x res` orthographic image looking
/// along azimuth `az_deg` around +Y (0 deg = front +Z facing camera).
fn rasterize_ortho_silhouette(
    tris: &[Tri],
    center: [f64; 3],
    span: f64,
    az_deg: f64,
    res: usize,
) -> Vec<bool> {
    let mut mask = vec![false; res * res];
    let az = az_deg.to_radians();
    let ca = az.cos();
    let sa = az.sin();
    // Camera right vector U and up vector V (+Y in glTF)
    let ux = ca;
    let uz = -sa;
    let inv_span = (res as f64) / span.max(1e-6);
    let half = 0.5 * (res as f64);

    let proj = |p: [f64; 3]| -> (f64, f64) {
        let dx = p[0] - center[0];
        let dy = p[1] - center[1];
        let dz = p[2] - center[2];
        let px = (dx * ux + dz * uz) * inv_span + half;
        // Row 0 = top (+Y)
        let py = half - dy * inv_span;
        (px, py)
    };

    for t in tris {
        let (x0, y0) = proj(t.p[0]);
        let (x1, y1) = proj(t.p[1]);
        let (x2, y2) = proj(t.p[2]);

        let min_x = x0.min(x1).min(x2).floor() as isize;
        let max_x = x0.max(x1).max(x2).ceil() as isize;
        let min_y = y0.min(y1).min(y2).floor() as isize;
        let max_y = y0.max(y1).max(y2).ceil() as isize;
        if max_x < 0 || max_y < 0 || min_x >= res as isize || min_y >= res as isize {
            continue;
        }
        let ix0 = min_x.max(0) as usize;
        let ix1 = max_x.min(res as isize - 1) as usize;
        let iy0 = min_y.max(0) as usize;
        let iy1 = max_y.min(res as isize - 1) as usize;

        let area = (x1 - x0) * (y2 - y0) - (y1 - y0) * (x2 - x0);
        if area.abs() < 1e-6 {
            continue;
        }
        let sign = if area > 0.0 { 1.0 } else { -1.0 };
        for py in iy0..=iy1 {
            let cy = py as f64 + 0.5;
            for px in ix0..=ix1 {
                let cx = px as f64 + 0.5;
                let e0 = ((x1 - x0) * (cy - y0) - (y1 - y0) * (cx - x0)) * sign;
                let e1 = ((x2 - x1) * (cy - y1) - (y2 - y1) * (cx - x1)) * sign;
                let e2 = ((x0 - x2) * (cy - y2) - (y0 - y2) * (cx - x2)) * sign;
                if e0 >= -1e-6 && e1 >= -1e-6 && e2 >= -1e-6 {
                    mask[py * res + px] = true;
                }
            }
        }
    }
    mask
}

fn extract_contour(mask: &[bool], w: usize, h: usize) -> Vec<bool> {
    let mut contour = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if !mask[y * w + x] {
                continue;
            }
            let on_edge = x == 0
                || y == 0
                || x + 1 == w
                || y + 1 == h
                || !mask[y * w + (x - 1)]
                || !mask[y * w + (x + 1)]
                || !mask[(y - 1) * w + x]
                || !mask[(y + 1) * w + x];
            if on_edge {
                contour[y * w + x] = true;
            }
        }
    }
    contour
}

/// Measure talk-camera silhouette contour drift (in pixels at 512x512 portrait framing)
/// across front, 3/4 right, 3/4 left, and profile views on head/chin/jaw/ears/tips.
fn check_silhouette_drift(cand: &[Tri], ref_tris: &[Tri], out: &mut Vec<(&'static str, String)>) {
    if cand.is_empty() || ref_tris.is_empty() {
        return;
    }
    let (rlo, rhi) = bbox(ref_tris);
    let h = rhi[1] - rlo[1];
    if h <= 1e-4 {
        return;
    }
    let cx = 0.5 * (rlo[0] + rhi[0]);
    let cz = 0.5 * (rlo[2] + rhi[2]);
    // If humanoid-proportioned (tall), frame talk-cam head/chin/jaw/ears/hair-tips window;
    // otherwise (e.g. standalone head fixture or prop), frame the whole object cleanly.
    let xw = (rhi[0] - rlo[0]).max(1e-6);
    let zw = (rhi[2] - rlo[2]).max(1e-6);
    let is_full_body = h > 1.35 * zw && h > 1.15 * xw;
    let (center, span) = if is_full_body {
        // Match talk-cam head framing (0.775..1.045 H: neck, chin, jaw, ears, hair tips —
        // excludes armpits/arms/lower cloak projecting into side profile below 0.77 H).
        ([cx, rlo[1] + 0.910 * h, cz], 0.27 * h)
    } else {
        ([cx, rlo[1] + 0.5 * h, cz], 1.12 * h.max(xw).max(zw))
    };

    let views = [
        ("front", 0.0f64),
        ("three_quarter_r", -35.0),
        ("three_quarter_l", 35.0),
        ("side_r", -85.0),
    ];
    let mut worst_p99 = 0.0f64;
    let mut worst_view = "";
    let mut worst_nick_px = 0.0f64;
    let mut worst_nick_cnt = 0usize;

    for (vname, az) in views {
        let r_mask = rasterize_ortho_silhouette(ref_tris, center, span, az, TALK_CAM_RES);
        let c_mask = rasterize_ortho_silhouette(cand, center, span, az, TALK_CAM_RES);
        let r_cont = extract_contour(&r_mask, TALK_CAM_RES, TALK_CAM_RES);
        let c_cont = extract_contour(&c_mask, TALK_CAM_RES, TALK_CAM_RES);
        let r_cnt = r_cont.iter().filter(|&&b| b).count();
        let c_cnt = c_cont.iter().filter(|&&b| b).count();
        if r_cnt < 16 || c_cnt < 16 {
            continue;
        }
        let dt_c = distance_transform_2d(&c_cont, TALK_CAM_RES, TALK_CAM_RES);
        let dt_r = distance_transform_2d(&r_cont, TALK_CAM_RES, TALK_CAM_RES);

        // Ignore bottom 4% of viewport where the neck/chest exits the talk-cam crop frame.
        let y_cut = (TALK_CAM_RES * 96) / 100;
        let mut dists: Vec<f64> = Vec::with_capacity(r_cnt + c_cnt);
        for y in 0..y_cut {
            for x in 0..TALK_CAM_RES {
                let idx = y * TALK_CAM_RES + x;
                if r_cont[idx] {
                    dists.push(dt_c[idx]);
                }
                if c_cont[idx] {
                    dists.push(dt_r[idx]);
                }
            }
        }
        if dists.is_empty() {
            continue;
        }
        dists.sort_by(|a, b| a.total_cmp(b));
        let p99 = dists[(dists.len() * 99 / 100).min(dists.len() - 1)];
        let over_2_cnt = dists.iter().filter(|&&d| d > 2.0).count();
        let max_d = *dists.last().unwrap_or(&0.0);
        if p99 > worst_p99 {
            worst_p99 = p99;
            worst_view = vname;
        }
        // Localized chin/ear/tip nick: >= 6 contour pixels drifting > 2.0 px.
        if over_2_cnt >= 6 && max_d > worst_nick_px {
            worst_nick_px = max_d;
            worst_nick_cnt = over_2_cnt;
            if worst_view.is_empty() {
                worst_view = vname;
            }
        }
    }

    if worst_p99 > SILHOUETTE_DRIFT_PX || (worst_nick_cnt >= 6 && worst_nick_px > 2.5) {
        out.push((
            "M_SILHOUETTE_DRIFT",
            format!(
                "talk-cam ({TALK_CAM_RES}px) silhouette drift exceeds {SILHOUETTE_DRIFT_PX:.1} px: \
                 p99={worst_p99:.2} px (worst view '{worst_view}', localized nick max={worst_nick_px:.2} px over {worst_nick_cnt} contour px)"
            ),
        ));
    }
}

/// Compare candidate GLB against a reference source GLB (`--reference`).
/// Emits `M_FOLD` (>35 deg off reference surface) and `M_SILHOUETTE_DRIFT` (>1.5 px talk-cam drift).
pub fn check_against_reference(
    cand_json: &Value,
    cand_bin: &[u8],
    ref_bytes: &[u8],
) -> Result<Vec<(&'static str, String)>, String> {
    let ref_doc = glb::parse(ref_bytes)
        .map_err(|(code, detail)| format!("invalid reference GLB ({code}): {detail}"))?;
    let ref_tris = extract_world_tris(&ref_doc.json, ref_doc.bin);
    let mut cand_tris = extract_world_tris(cand_json, cand_bin);
    if ref_tris.is_empty() || cand_tris.is_empty() {
        return Ok(Vec::new());
    }
    align_if_different_frame(&mut cand_tris, &ref_tris);
    let mut out = Vec::new();
    check_reference_folds(&cand_tris, &ref_tris, &mut out);
    check_silhouette_drift(&cand_tris, &ref_tris, &mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::AssetClass;
    use crate::checks;
    use serde_json::json;

    fn pack_bin(doc: &Value, bin: &[u8]) -> Vec<u8> {
        let mut js = doc.to_string().into_bytes();
        while !js.len().is_multiple_of(4) {
            js.push(b' ');
        }
        let mut bd = bin.to_vec();
        while !bd.len().is_multiple_of(4) {
            bd.push(0);
        }
        let total = 12 + 8 + js.len() + 8 + bd.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&0x46546C67u32.to_le_bytes());
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(js.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x4E4F534Au32.to_le_bytes());
        out.extend_from_slice(&js);
        out.extend_from_slice(&(bd.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x004E4942u32.to_le_bytes());
        out.extend_from_slice(&bd);
        out
    }

    /// Build a closed UV sphere (optionally with a planted chin nick or planted >35 deg surface fold).
    fn build_head_sphere(nick_chin: bool, plant_fold: bool) -> Vec<u8> {
        let rings = 24usize;
        let segs = 32usize;
        let mut pos: Vec<[f32; 3]> = Vec::new();
        let mut idx: Vec<u16> = Vec::new();
        let cy = 1.55f32;
        let rx = 0.10f32;
        let ry = 0.13f32;
        let rz = 0.11f32;

        // Top pole
        pos.push([0.0, cy + ry, 0.0]);
        for r in 1..rings {
            let phi = std::f32::consts::PI * (r as f32) / (rings as f32);
            let sp = phi.sin();
            let cp = phi.cos();
            for s in 0..segs {
                let th = 2.0 * std::f32::consts::PI * (s as f32) / (segs as f32);
                let mut x = rx * sp * th.sin();
                let mut y = cy + ry * cp;
                let mut z = rz * sp * th.cos();
                // Plant a localized chin nick on lower front (+Z, low Y)
                if nick_chin && z > 0.04 && y < cy - 0.065 && x.abs() < 0.045 {
                    z -= 0.025;
                    y += 0.018;
                }
                // Plant a steep >50 deg ridge fold across cheek patch
                if plant_fold && z > 0.06 && (y - cy).abs() < 0.045 && x > 0.01 && x < 0.06 {
                    let wave = if (r + s) % 2 == 0 { 0.016 } else { -0.016 };
                    z += wave;
                    x += wave * 0.5;
                }
                pos.push([x, y, z]);
            }
        }
        // Bottom pole
        pos.push([0.0, cy - ry, 0.0]);
        let bot = (pos.len() - 1) as u16;

        // Top cap
        for s in 0..segs {
            let a = 1 + s;
            let b = 1 + (s + 1) % segs;
            idx.extend_from_slice(&[0, a as u16, b as u16]);
        }
        // Middle bands
        for r in 0..(rings - 2) {
            let row0 = 1 + r * segs;
            let row1 = 1 + (r + 1) * segs;
            for s in 0..segs {
                let s1 = (s + 1) % segs;
                let a = (row0 + s) as u16;
                let b = (row1 + s) as u16;
                let c = (row1 + s1) as u16;
                let d = (row0 + s1) as u16;
                idx.extend_from_slice(&[a, b, c, a, c, d]);
            }
        }
        // Bottom cap
        let last_row = 1 + (rings - 2) * segs;
        for s in 0..segs {
            let a = (last_row + s) as u16;
            let b = (last_row + (s + 1) % segs) as u16;
            idx.extend_from_slice(&[a, bot, b]);
        }

        let mut bin = Vec::new();
        for p in &pos {
            for &x in p {
                bin.extend_from_slice(&x.to_le_bytes());
            }
        }
        let pos_len = bin.len();
        let idx_off = bin.len();
        for &i in &idx {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        let idx_len = bin.len() - idx_off;

        let doc = json!({
            "asset": {"version": "2.0"},
            "scenes": [{"nodes": [0]}],
            "nodes": [{"name": "Head", "mesh": 0}],
            "meshes": [{"name": "Head", "primitives": [{"attributes": {"POSITION": 0}, "indices": 1}]}],
            "buffers": [{"byteLength": bin.len()}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": pos_len},
                {"buffer": 0, "byteOffset": idx_off, "byteLength": idx_len},
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": pos.len(), "type": "VEC3"},
                {"bufferView": 1, "componentType": 5123, "count": idx.len(), "type": "SCALAR"},
            ],
        });
        pack_bin(&doc, &bin)
    }

    #[test]
    fn clean_reference_match_passes() {
        let ref_glb = build_head_sphere(false, false);
        let opts = checks::CheckOpts {
            strict_mesh: true,
            reference: Some(&ref_glb),
            ..Default::default()
        };
        let r = checks::check_glb_with_opts(&ref_glb, None, Some(AssetClass::Prop), &opts);
        assert!(
            r.diags.is_empty(),
            "clean head against itself should pass: {:?}",
            r.diags
                .iter()
                .map(|d| (d.code, &d.detail))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn planted_nicked_chin_fails_silhouette_drift() {
        let ref_glb = build_head_sphere(false, false);
        let nicked = build_head_sphere(true, false);
        let opts = checks::CheckOpts {
            strict_mesh: true,
            reference: Some(&ref_glb),
            ..Default::default()
        };
        let r = checks::check_glb_with_opts(&nicked, None, Some(AssetClass::Prop), &opts);
        assert!(
            r.diags.iter().any(|d| d.code == "M_SILHOUETTE_DRIFT"),
            "planted nicked chin must trigger M_SILHOUETTE_DRIFT, got: {:?}",
            r.diags
                .iter()
                .map(|d| (d.code, &d.detail))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn planted_surface_fold_fails_reference_fold_check() {
        let ref_glb = build_head_sphere(false, false);
        let folded = build_head_sphere(false, true);
        let opts = checks::CheckOpts {
            strict_mesh: true,
            reference: Some(&ref_glb),
            ..Default::default()
        };
        let r = checks::check_glb_with_opts(&folded, None, Some(AssetClass::Prop), &opts);
        assert!(
            r.diags.iter().any(|d| d.code == "M_FOLD"),
            "planted >35 deg fold must trigger M_FOLD, got: {:?}",
            r.diags
                .iter()
                .map(|d| (d.code, &d.detail))
                .collect::<Vec<_>>()
        );
    }
}
