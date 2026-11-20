// SPDX-License-Identifier: MIT
//! Mesh-geometry checks (M_* layer).
//!
//! Catches two kinds of inverted geometry under `M_FLIPPED_NORMAL`:
//! 1. **Normal-vs-winding disagreement:** triangles whose exported vertex
//!    normals oppose the geometric face normal `(p1 - p0) x (p2 - p0)`.
//! 2. **Inside-out closed shells:** watertight connected components whose
//!    signed volume is negative (catches the classic case where both winding
//!    and vertex normals were flipped together).
//!
//! Unreadable, non-triangle, or sparse primitives are skipped cleanly without
//! panicking.

use crate::util::{arr, as_idx, mesh_tag};
use crate::weights;
use glbkit::accessor::{Component, Layout};
use serde_json::Value;
use std::collections::HashMap;

/// Minimum cross-product magnitude for a triangle to be considered non-degenerate.
const MIN_AREA2: f64 = 1e-12;
/// Minimum normal length to avoid dividing by zero on unset normals.
const MIN_NORM: f64 = 1e-8;
/// Per-vertex dot-product threshold below which a vertex normal strictly opposes
/// the geometric face normal (avoids grazing ~90° smooth-shading creases).
const STRICT_NEG_DOT: f64 = -0.1;
/// Quantization scale (0.1 mm) for welding coincident split vertices (UV/hard-edge
/// seams) when testing whether a connected shell is watertight/closed.
const WELD_SCALE: f64 = 10_000.0;
/// Minimum negative signed volume magnitude to flag an inside-out closed shell.
const MIN_NEG_VOLUME: f64 = 1e-9;

#[inline]
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
fn len(v: [f64; 3]) -> f64 {
    dot(v, v).sqrt()
}

/// True if any node referencing `mesh_idx` carries a negative-determinant local
/// transform (reflection / mirrored scale). In glTF 2.0 such nodes legitimately
/// flip winding at draw time, so mesh-local signed volume must not be flagged.
fn mesh_has_mirrored_node(json: &Value, mesh_idx: usize) -> bool {
    let nodes = arr(json, "nodes");
    // Build child -> parent map to accumulate ancestor reflection parity.
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
    let node_neg = |n: &Value| -> bool {
        if let Some(m) = n.get("matrix").and_then(Value::as_array) {
            if m.len() == 16 {
                let g = |i: usize| m[i].as_f64().unwrap_or(0.0);
                // Column-major 4x4: upper-left 3x3 determinant.
                let c0 = [g(0), g(1), g(2)];
                let c1 = [g(4), g(5), g(6)];
                let c2 = [g(8), g(9), g(10)];
                return dot(c0, cross(c1, c2)) < 0.0;
            }
        }
        if let Some(s) = n.get("scale").and_then(Value::as_array) {
            if s.len() == 3 {
                let prod = s[0].as_f64().unwrap_or(1.0)
                    * s[1].as_f64().unwrap_or(1.0)
                    * s[2].as_f64().unwrap_or(1.0);
                return prod < 0.0;
            }
        }
        false
    };
    for (ni, n) in nodes.iter().enumerate() {
        if n.get("mesh").and_then(as_idx) != Some(mesh_idx) {
            continue;
        }
        let mut neg = false;
        let mut cur = Some(ni);
        let mut hops = 0usize;
        while let Some(idx) = cur {
            if hops > nodes.len() {
                break;
            }
            if node_neg(&nodes[idx]) {
                neg = !neg;
            }
            cur = parent[idx];
            hops += 1;
        }
        if neg {
            return true;
        }
    }
    false
}

#[derive(Default)]
struct NormalCounts {
    total: usize,
    flipped_avg: usize,
    flipped_all: usize,
}

struct ShellTri {
    w: [usize; 3],
    pa: [f64; 3],
    pb: [f64; 3],
    pc: [f64; 3],
}

#[derive(Default)]
struct ShellBuilder {
    welded: HashMap<[i64; 3], usize>,
    parent: Vec<usize>,
    tris: Vec<ShellTri>,
}

impl ShellBuilder {
    fn weld(&mut self, p: [f64; 3]) -> usize {
        let key = [
            (p[0] * WELD_SCALE).round() as i64,
            (p[1] * WELD_SCALE).round() as i64,
            (p[2] * WELD_SCALE).round() as i64,
        ];
        if let Some(&id) = self.welded.get(&key) {
            return id;
        }
        let id = self.parent.len();
        self.parent.push(id);
        self.welded.insert(key, id);
        id
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[ra] = rb;
        }
    }

    fn push(&mut self, pa: [f64; 3], pb: [f64; 3], pc: [f64; 3]) {
        let wa = self.weld(pa);
        let wb = self.weld(pb);
        let wc = self.weld(pc);
        if wa == wb || wb == wc || wc == wa {
            return;
        }
        self.union(wa, wb);
        self.union(wb, wc);
        self.tris.push(ShellTri {
            w: [wa, wb, wc],
            pa,
            pb,
            pc,
        });
    }

    /// Return `(inside_out_shell_count, inside_out_tri_count)` for closed
    /// (watertight) connected components whose signed volume is strictly negative.
    fn inside_out_closed_shells(&mut self) -> (usize, usize) {
        if self.tris.len() < 4 {
            return (0, 0);
        }
        struct Comp {
            origin: [f64; 3],
            tris: usize,
            vol6: f64,
            edges: HashMap<(usize, usize), i32>,
        }
        let mut comps: HashMap<usize, Comp> = HashMap::new();
        let n = self.tris.len();
        for i in 0..n {
            let w = self.tris[i].w;
            let pa = self.tris[i].pa;
            let pb = self.tris[i].pb;
            let pc = self.tris[i].pc;
            let root = self.find(w[0]);
            let comp = comps.entry(root).or_insert_with(|| Comp {
                origin: pa,
                tris: 0,
                vol6: 0.0,
                edges: HashMap::new(),
            });
            comp.tris += 1;
            let a = sub(pa, comp.origin);
            let b = sub(pb, comp.origin);
            let c = sub(pc, comp.origin);
            comp.vol6 += dot(a, cross(b, c));
            for (u, v) in [(w[0], w[1]), (w[1], w[2]), (w[2], w[0])] {
                if u < v {
                    *comp.edges.entry((u, v)).or_insert(0) += 1;
                } else {
                    *comp.edges.entry((v, u)).or_insert(0) -= 1;
                }
            }
        }
        let mut neg_shells = 0usize;
        let mut neg_tris = 0usize;
        for comp in comps.values() {
            if comp.tris >= 4
                && comp.edges.values().all(|&bal| bal == 0)
                && (comp.vol6 / 6.0) < -MIN_NEG_VOLUME
            {
                neg_shells += 1;
                neg_tris += comp.tris;
            }
        }
        (neg_shells, neg_tris)
    }
}

#[inline]
fn process_triangle(
    bin: &[u8],
    pos: &Layout,
    nrm: Option<&Layout>,
    tri: [usize; 3],
    counts: &mut NormalCounts,
    shells: &mut Option<ShellBuilder>,
) {
    let [a, b, c] = tri;
    let (Some(pa), Some(pb), Some(pc)) = (
        read_vec3(bin, pos, a),
        read_vec3(bin, pos, b),
        read_vec3(bin, pos, c),
    ) else {
        return;
    };
    let face = cross(sub(pb, pa), sub(pc, pa));
    let fl = len(face);
    if !fl.is_finite() || fl <= MIN_AREA2 {
        return;
    }
    if let Some(sb) = shells.as_mut() {
        sb.push(pa, pb, pc);
    }

    let Some(nrm) = nrm else {
        counts.total += 1;
        return;
    };
    let fnrm = [face[0] / fl, face[1] / fl, face[2] / fl];
    let (Some(na), Some(nb), Some(nc)) = (
        read_vec3(bin, nrm, a),
        read_vec3(bin, nrm, b),
        read_vec3(bin, nrm, c),
    ) else {
        return;
    };
    let avg = [
        na[0] + nb[0] + nc[0],
        na[1] + nb[1] + nc[1],
        na[2] + nb[2] + nc[2],
    ];
    let al = len(avg);
    if !al.is_finite() || al <= MIN_NORM {
        return;
    }

    counts.total += 1;
    let avg_dot = dot(fnrm, avg) / al;
    if avg_dot < 0.0 {
        counts.flipped_avg += 1;
        let la = len(na);
        let lb = len(nb);
        let lc = len(nc);
        if la > MIN_NORM && lb > MIN_NORM && lc > MIN_NORM {
            let da = dot(fnrm, na) / la;
            let db = dot(fnrm, nb) / lb;
            let dc = dot(fnrm, nc) / lc;
            if da < STRICT_NEG_DOT && db < STRICT_NEG_DOT && dc < STRICT_NEG_DOT {
                counts.flipped_all += 1;
            }
        }
    }
}

fn is_sparse(json: &Value, ai: usize) -> bool {
    arr(json, "accessors")
        .get(ai)
        .and_then(|a| a.get("sparse"))
        .is_some()
}

/// Flag triangle primitives with inverted normals or inside-out closed shells.
///
/// 1. Normal-vs-winding disagreement (when `NORMAL` is present): fires when
///    either >= 0.2% (and >= 1) of valid triangles have all 3 vertex normals
///    strictly opposing the geometric face normal (`dot < -0.1`), or >= 2%
///    (and >= 10) have opposing average normal alongside >= 5 strictly inverted
///    triangles.
/// 2. Inside-out closed shell (unless the mesh is instantiated via a mirrored
///    negative-determinant node): fires when any watertight connected component
///    (>= 4 triangles) has strictly negative signed volume.
pub fn check_mesh(json: &Value, bin: &[u8]) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let meshes = arr(json, "meshes");
    for (mi, mesh) in meshes.iter().enumerate() {
        let mtag = mesh_tag(meshes, mi);
        let mirrored = mesh_has_mirrored_node(json, mi);
        for (pi, prim) in arr(mesh, "primitives").iter().enumerate() {
            let mode = prim.get("mode").and_then(Value::as_u64).unwrap_or(4);
            if mode != 4 && mode != 5 {
                continue;
            }
            let attrs = match prim.get("attributes") {
                Some(a) => a,
                None => continue,
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

            let nrm: Option<Layout> = match attrs.get("NORMAL").and_then(as_idx) {
                Some(ni) if !is_sparse(json, ni) => {
                    match weights::layout_of(json, bin.len(), ni, "NORMAL") {
                        Ok(l)
                            if l.component == Component::F32
                                && l.width == 3
                                && l.count == pos.count =>
                        {
                            Some(l)
                        }
                        _ => None,
                    }
                }
                _ => None,
            };

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

            let mut counts = NormalCounts::default();
            let mut shells = if mirrored {
                None
            } else {
                Some(ShellBuilder::default())
            };

            if mode == 4 {
                let mut k = 0usize;
                while k + 2 < n_idx {
                    if let (Some(a), Some(b), Some(c)) =
                        (read_idx(k), read_idx(k + 1), read_idx(k + 2))
                    {
                        if a < pos.count && b < pos.count && c < pos.count {
                            process_triangle(
                                bin,
                                &pos,
                                nrm.as_ref(),
                                [a, b, c],
                                &mut counts,
                                &mut shells,
                            );
                        }
                    }
                    k += 3;
                }
            } else {
                // TRIANGLE_STRIP (mode 5): odd triangles flip winding to match the strip.
                for k in 0..n_idx - 2 {
                    if let (Some(a), Some(b), Some(c)) =
                        (read_idx(k), read_idx(k + 1), read_idx(k + 2))
                    {
                        if a < pos.count && b < pos.count && c < pos.count {
                            let tri = if k % 2 == 0 { [a, b, c] } else { [a, c, b] };
                            process_triangle(
                                bin,
                                &pos,
                                nrm.as_ref(),
                                tri,
                                &mut counts,
                                &mut shells,
                            );
                        }
                    }
                }
            }

            if counts.total == 0 {
                continue;
            }
            let NormalCounts {
                total,
                flipped_avg,
                flipped_all,
            } = counts;
            let bad_all =
                flipped_all >= 1 && flipped_all.saturating_mul(1000) >= total.saturating_mul(2);
            let bad_avg = flipped_avg >= 10
                && flipped_avg.saturating_mul(100) >= total.saturating_mul(2)
                && flipped_all >= 5;
            if nrm.is_some() && (bad_all || bad_avg) {
                let pct = (flipped_avg as f64) * 100.0 / (total as f64);
                out.push((
                    "M_FLIPPED_NORMAL",
                    format!(
                        "{mtag} prim {pi}: {flipped_avg} of {total} triangles ({pct:.1}%) \
                         have vertex normals opposing geometric face normal \
                         ({flipped_all} fully inverted)"
                    ),
                ));
            }

            if let Some(mut sb) = shells {
                let (neg_shells, neg_tris) = sb.inside_out_closed_shells();
                if neg_shells > 0 {
                    out.push((
                        "M_FLIPPED_NORMAL",
                        format!(
                            "{mtag} prim {pi}: {neg_shells} closed shell{} ({neg_tris} of {total} \
                             triangles) inside-out (negative signed volume)",
                            if neg_shells == 1 { "" } else { "s" },
                        ),
                    ));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::budget::AssetClass;
    use crate::checks;
    use serde_json::{json, Value};

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

    fn build_mesh_glb(
        positions: &[[f32; 3]],
        normals: &[[f32; 3]],
        indices: Option<&[u16]>,
        mode: Option<u64>,
        double_sided: bool,
        node_scale: Option<[f32; 3]>,
    ) -> Vec<u8> {
        let mut bin = Vec::new();
        for p in positions {
            for &x in p {
                bin.extend_from_slice(&x.to_le_bytes());
            }
        }
        let pos_len = bin.len();
        let nrm_off = bin.len();
        for n in normals {
            for &x in n {
                bin.extend_from_slice(&x.to_le_bytes());
            }
        }
        let nrm_len = bin.len() - nrm_off;

        let mut views = vec![
            json!({"buffer": 0, "byteOffset": 0, "byteLength": pos_len}),
            json!({"buffer": 0, "byteOffset": nrm_off, "byteLength": nrm_len}),
        ];
        let mut accs = vec![
            json!({"bufferView": 0, "componentType": 5126, "count": positions.len(), "type": "VEC3"}),
            json!({"bufferView": 1, "componentType": 5126, "count": normals.len(), "type": "VEC3"}),
        ];
        let mut prim = json!({
            "attributes": {"POSITION": 0, "NORMAL": 1},
            "material": 0
        });
        if let Some(m) = mode {
            prim["mode"] = json!(m);
        }
        if let Some(inds) = indices {
            let idx_off = bin.len();
            for &i in inds {
                bin.extend_from_slice(&i.to_le_bytes());
            }
            let idx_len = bin.len() - idx_off;
            views.push(json!({"buffer": 0, "byteOffset": idx_off, "byteLength": idx_len}));
            accs.push(json!({"bufferView": 2, "componentType": 5123, "count": inds.len(), "type": "SCALAR"}));
            prim["indices"] = json!(2);
        }
        let mut node = json!({"name": "Mesh", "mesh": 0});
        if let Some(s) = node_scale {
            node["scale"] = json!(s);
        }
        let doc = json!({
            "asset": {"version": "2.0"},
            "scenes": [{"nodes": [0]}],
            "nodes": [node],
            "materials": [{"name": "Mat", "doubleSided": double_sided}],
            "meshes": [{"name": "TestMesh", "primitives": [prim]}],
            "buffers": [{"byteLength": bin.len()}],
            "bufferViews": views,
            "accessors": accs,
        });
        pack_bin(&doc, &bin)
    }

    fn unit_cube(inside_out: bool) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u16>) {
        // 6 faces, 4 unique vertices per face (tests vertex welding across hard-edge seams too).
        let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
            (
                [0.0, 0.0, 1.0],
                [
                    [0.0, 0.0, 1.0],
                    [1.0, 0.0, 1.0],
                    [1.0, 1.0, 1.0],
                    [0.0, 1.0, 1.0],
                ],
            ),
            (
                [0.0, 0.0, -1.0],
                [
                    [0.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    [1.0, 1.0, 0.0],
                    [1.0, 0.0, 0.0],
                ],
            ),
            (
                [1.0, 0.0, 0.0],
                [
                    [1.0, 0.0, 0.0],
                    [1.0, 1.0, 0.0],
                    [1.0, 1.0, 1.0],
                    [1.0, 0.0, 1.0],
                ],
            ),
            (
                [-1.0, 0.0, 0.0],
                [
                    [0.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0],
                    [0.0, 1.0, 1.0],
                    [0.0, 1.0, 0.0],
                ],
            ),
            (
                [0.0, 1.0, 0.0],
                [
                    [0.0, 1.0, 0.0],
                    [0.0, 1.0, 1.0],
                    [1.0, 1.0, 1.0],
                    [1.0, 1.0, 0.0],
                ],
            ),
            (
                [0.0, -1.0, 0.0],
                [
                    [0.0, 0.0, 0.0],
                    [1.0, 0.0, 0.0],
                    [1.0, 0.0, 1.0],
                    [0.0, 0.0, 1.0],
                ],
            ),
        ];
        let mut pos = Vec::new();
        let mut nrm = Vec::new();
        let mut idx = Vec::new();
        for (n, verts) in faces {
            let base = pos.len() as u16;
            let nn = if inside_out { [-n[0], -n[1], -n[2]] } else { n };
            pos.extend_from_slice(&verts);
            nrm.extend_from_slice(&[nn; 4]);
            if inside_out {
                idx.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
            } else {
                idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        }
        (pos, nrm, idx)
    }

    fn codes(bytes: &[u8]) -> Vec<&'static str> {
        let r = checks::check_glb_with_class(bytes, None, Some(AssetClass::Prop));
        r.diags.iter().map(|d| d.code).collect()
    }

    #[test]
    fn clean_triangles_pass() {
        let pos = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let nrm = [[0.0, 0.0, 1.0]; 3];
        let glb = build_mesh_glb(&pos, &nrm, Some(&[0, 1, 2]), None, false, None);
        assert!(!codes(&glb).contains(&"M_FLIPPED_NORMAL"));
    }

    #[test]
    fn flipped_winding_flags() {
        let pos = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let nrm = [[0.0, 0.0, 1.0]; 3];
        let glb = build_mesh_glb(&pos, &nrm, Some(&[0, 2, 1]), None, false, None);
        let r = checks::check_glb_with_class(&glb, None, Some(AssetClass::Prop));
        let d = r
            .diags
            .iter()
            .find(|d| d.code == "M_FLIPPED_NORMAL")
            .expect("expected M_FLIPPED_NORMAL");
        assert!(d.detail.contains("1 of 1 triangles"), "{}", d.detail);
        assert!(d.detail.contains("1 fully inverted"), "{}", d.detail);
    }

    #[test]
    fn double_sided_inverted_normals_still_flags() {
        let pos = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let nrm = [[0.0, 0.0, -1.0]; 3];
        let glb = build_mesh_glb(&pos, &nrm, None, Some(4), true, None);
        assert!(codes(&glb).contains(&"M_FLIPPED_NORMAL"));
    }

    #[test]
    fn triangle_strip_winding_respected() {
        let pos = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
        ];
        let nrm = [[0.0, 0.0, 1.0]; 4];
        let clean = build_mesh_glb(&pos, &nrm, Some(&[0, 1, 2, 3]), Some(5), false, None);
        assert!(!codes(&clean).contains(&"M_FLIPPED_NORMAL"));

        let bad_nrm = [[0.0, 0.0, -1.0]; 4];
        let bad = build_mesh_glb(&pos, &bad_nrm, Some(&[0, 1, 2, 3]), Some(5), false, None);
        assert!(codes(&bad).contains(&"M_FLIPPED_NORMAL"));
    }

    #[test]
    fn degenerate_triangles_ignored() {
        let pos = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]];
        let nrm = [[0.0, 0.0, -1.0]; 3];
        let glb = build_mesh_glb(&pos, &nrm, Some(&[0, 1, 2]), None, false, None);
        assert!(!codes(&glb).contains(&"M_FLIPPED_NORMAL"));
    }

    #[test]
    fn isolated_smooth_crease_in_large_mesh_passes() {
        let mut pos = Vec::new();
        let mut nrm = Vec::new();
        for i in 0..300 {
            let x = i as f32;
            pos.extend_from_slice(&[[x, 0.0, 0.0], [x + 1.0, 0.0, 0.0], [x, 1.0, 0.0]]);
            nrm.extend_from_slice(&[[0.0, 0.0, 1.0]; 3]);
        }
        pos.extend_from_slice(&[[400.0, 0.0, 0.0], [401.0, 0.0, 0.0], [400.0, 1.0, 0.0]]);
        nrm.extend_from_slice(&[[0.0, 0.0, 0.2], [0.0, 0.9, -0.3], [0.0, 0.9, -0.3]]);
        let glb = build_mesh_glb(&pos, &nrm, None, Some(4), true, None);
        assert!(!codes(&glb).contains(&"M_FLIPPED_NORMAL"));
    }

    #[test]
    fn threshold_boundary_at_two_per_mille() {
        let make = |n_flipped: usize| {
            let mut pos = Vec::new();
            let mut nrm = Vec::new();
            for i in 0..1000 {
                let x = i as f32;
                pos.extend_from_slice(&[[x, 0.0, 0.0], [x + 1.0, 0.0, 0.0], [x, 1.0, 0.0]]);
                let nz = if i < n_flipped { -1.0 } else { 1.0 };
                nrm.extend_from_slice(&[[0.0, 0.0, nz]; 3]);
            }
            build_mesh_glb(&pos, &nrm, None, Some(4), false, None)
        };
        // 1/1000 = 0.1% is just below the 0.2% threshold -> passes.
        assert!(!codes(&make(1)).contains(&"M_FLIPPED_NORMAL"));
        // 2/1000 = 0.2% hits the boundary -> flags M_FLIPPED_NORMAL.
        assert!(codes(&make(2)).contains(&"M_FLIPPED_NORMAL"));
    }

    #[test]
    fn inside_out_closed_shell_with_matching_normals_flags() {
        // Outward closed cube passes cleanly.
        let (pos_ok, nrm_ok, idx_ok) = unit_cube(false);
        let ok_glb = build_mesh_glb(&pos_ok, &nrm_ok, Some(&idx_ok), None, false, None);
        assert!(!codes(&ok_glb).contains(&"M_FLIPPED_NORMAL"));

        // Inside-out closed cube (BOTH winding and normals inverted together!) must still fail.
        let (pos_bad, nrm_bad, idx_bad) = unit_cube(true);
        let bad_glb = build_mesh_glb(&pos_bad, &nrm_bad, Some(&idx_bad), None, false, None);
        let r = checks::check_glb_with_class(&bad_glb, None, Some(AssetClass::Prop));
        let d = r
            .diags
            .iter()
            .find(|d| d.code == "M_FLIPPED_NORMAL")
            .expect("expected M_FLIPPED_NORMAL for inside-out closed shell");
        assert!(d.detail.contains("inside-out"), "{}", d.detail);
        assert!(d.detail.contains("12 of 12 triangles"), "{}", d.detail);
    }

    #[test]
    fn mirrored_node_negative_scale_skips_signed_volume_false_alarm() {
        let (pos_bad, nrm_bad, idx_bad) = unit_cube(true);
        let mirrored_glb = build_mesh_glb(
            &pos_bad,
            &nrm_bad,
            Some(&idx_bad),
            None,
            false,
            Some([-1.0, 1.0, 1.0]),
        );
        assert!(!codes(&mirrored_glb).contains(&"M_FLIPPED_NORMAL"));
    }
}
