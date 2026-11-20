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
use std::collections::{BTreeMap, HashMap};

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

fn is_intentional_inward_shell(mesh_name: &str) -> bool {
    let lower = mesh_name.to_ascii_lowercase();
    lower.contains("skybox")
        || lower.contains("skydome")
        || lower.contains("outline")
        || lower.contains("inverted_hull")
}

/// Flag triangle primitives with inverted normals or inside-out closed shells.
///
/// 1. Normal-vs-winding disagreement (when `NORMAL` is present): fires when
///    either >= 0.2% (and >= 2, or 100% on tiny 1-tri meshes) of valid
///    triangles have all 3 vertex normals strictly opposing the geometric face
///    normal (`dot < -0.1`), or >= 2% (and >= 10) have opposing average normal
///    alongside >= 5 strictly inverted triangles.
/// 2. Inside-out closed shell across all primitives of a mesh (unless the mesh
///    is instantiated via a mirrored negative-determinant node or explicitly
///    named as an inward shell such as `skybox`/`skydome`/`outline`/`inverted_hull`):
///    fires when any watertight connected component (>= 4 triangles) has
///    strictly negative signed volume.
pub fn check_mesh(json: &Value, bin: &[u8]) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let meshes = arr(json, "meshes");
    for (mi, mesh) in meshes.iter().enumerate() {
        let mtag = mesh_tag(meshes, mi);
        let mirrored = mesh_has_mirrored_node(json, mi);
        let skip_inward = mirrored || is_intentional_inward_shell(&mtag);
        let mut mesh_shells = if skip_inward {
            None
        } else {
            Some(ShellBuilder::default())
        };
        let mut mesh_total_tris = 0usize;
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
                                &mut mesh_shells,
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
                                &mut mesh_shells,
                            );
                        }
                    }
                }
            }

            if counts.total == 0 {
                continue;
            }
            mesh_total_tris += counts.total;
            let NormalCounts {
                total,
                flipped_avg,
                flipped_all,
            } = counts;
            let bad_all = (flipped_all >= 2 || (total == 1 && flipped_all == 1))
                && flipped_all.saturating_mul(1000) >= total.saturating_mul(2);
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
        }

        if let Some(mut sb) = mesh_shells {
            let (neg_shells, neg_tris) = sb.inside_out_closed_shells();
            if neg_shells > 0 {
                out.push((
                    "M_FLIPPED_NORMAL",
                    format!(
                        "{mtag}: {neg_shells} closed shell{} ({neg_tris} of {mesh_total_tris} \
                         triangles) inside-out (negative signed volume)",
                        if neg_shells == 1 { "" } else { "s" },
                    ),
                ));
            }
        }
    }
    out
}

/// Sliver minimum interior angle threshold in degrees.
const SLIVER_MIN_ANGLE_DEG: f64 = 8.0;
/// Self-fold dihedral normal dot threshold (`dot < -0.50` => normal bend > 120 deg,
/// i.e. interior dihedral angle < 60 deg — catches acute fold-back pinches without
/// false-alarming on 75-90 deg cloth collars, hair valleys, or architectural corners).
const SELF_FOLD_MAX_DOT: f64 = -0.50;

#[inline]
fn read_vec2(bin: &[u8], lay: &Layout, idx: usize) -> Option<[f64; 2]> {
    let u = f64::from(weights::f32_at(bin, lay, idx, 0)?);
    let v = f64::from(weights::f32_at(bin, lay, idx, 1)?);
    if u.is_finite() && v.is_finite() {
        Some([u, v])
    } else {
        None
    }
}

fn is_intentional_open_sheet(mesh_name: &str) -> bool {
    let lower = mesh_name.to_ascii_lowercase();
    [
        "hair",
        "cape",
        "cloak",
        "leaf",
        "leaves",
        "foliage",
        "grass",
        "plane",
        "ground",
        "terrain",
        "skybox",
        "skydome",
        "card",
        "ribbon",
        "cloth",
        "curtain",
        "water",
        "billboard",
        "eyelash",
        "lash",
        "brow",
        "outline",
        "inverted_hull",
    ]
    .iter()
    .any(|w| lower.contains(w))
}

fn min_tri_angle_deg(pa: [f64; 3], pb: [f64; 3], pc: [f64; 3]) -> f64 {
    let e0 = sub(pb, pa);
    let e1 = sub(pc, pb);
    let e2 = sub(pa, pc);
    let l0 = len(e0);
    let l1 = len(e1);
    let l2 = len(e2);
    if l0 <= 1e-12 || l1 <= 1e-12 || l2 <= 1e-12 {
        return 0.0;
    }
    let c0 = (-dot(e0, e2) / (l0 * l2)).clamp(-1.0, 1.0);
    let c1 = (-dot(e0, e1) / (l0 * l1)).clamp(-1.0, 1.0);
    let c2 = (-dot(e1, e2) / (l1 * l2)).clamp(-1.0, 1.0);
    // Larger cosine = smaller angle
    c0.max(c1).max(c2).acos().to_degrees()
}

struct HygieneEdge {
    count: usize,
    bal: i32,
    normals: [[f64; 3]; 2],
    mid_y: f64,
}

struct HygieneBuilder {
    welded: BTreeMap<[i64; 3], usize>,
    parent: Vec<usize>,
    edges: BTreeMap<(usize, usize), HygieneEdge>,
    tri_comp_root: Vec<usize>,
    tri_area: Vec<f64>,
    valid_tris: usize,
    degenerate_tris: usize,
    sliver_tris: usize,
    neck_sliver_tris: usize,
    worst_sliver_deg: f64,
    min_y: f64,
    max_y: f64,
    min_x: f64,
    max_x: f64,
    min_z: f64,
    max_z: f64,
    tri_centers_y: Vec<f64>,
    tri_is_sliver: Vec<bool>,
}

impl Default for HygieneBuilder {
    fn default() -> Self {
        Self {
            welded: BTreeMap::new(),
            parent: Vec::new(),
            edges: BTreeMap::new(),
            tri_comp_root: Vec::new(),
            tri_area: Vec::new(),
            valid_tris: 0,
            degenerate_tris: 0,
            sliver_tris: 0,
            neck_sliver_tris: 0,
            worst_sliver_deg: 180.0,
            min_y: f64::INFINITY,
            max_y: f64::NEG_INFINITY,
            min_x: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            min_z: f64::INFINITY,
            max_z: f64::NEG_INFINITY,
            tri_centers_y: Vec::new(),
            tri_is_sliver: Vec::new(),
        }
    }
}

impl HygieneBuilder {
    fn weld(&mut self, p: [f64; 3]) -> usize {
        self.min_x = self.min_x.min(p[0]);
        self.max_x = self.max_x.max(p[0]);
        self.min_y = self.min_y.min(p[1]);
        self.max_y = self.max_y.max(p[1]);
        self.min_z = self.min_z.min(p[2]);
        self.max_z = self.max_z.max(p[2]);
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

    fn push_tri(&mut self, pa: [f64; 3], pb: [f64; 3], pc: [f64; 3]) {
        let wa = self.weld(pa);
        let wb = self.weld(pb);
        let wc = self.weld(pc);
        if wa == wb || wb == wc || wc == wa {
            self.degenerate_tris += 1;
            return;
        }
        let face = cross(sub(pb, pa), sub(pc, pa));
        let fl = len(face);
        if !fl.is_finite() || fl <= MIN_AREA2 {
            self.degenerate_tris += 1;
            return;
        }
        self.valid_tris += 1;
        let area = 0.5 * fl;
        let fnrm = [face[0] / fl, face[1] / fl, face[2] / fl];

        let min_ang = min_tri_angle_deg(pa, pb, pc);
        let is_sliver = min_ang < SLIVER_MIN_ANGLE_DEG;
        if is_sliver {
            self.sliver_tris += 1;
            if min_ang < self.worst_sliver_deg {
                self.worst_sliver_deg = min_ang;
            }
        }
        self.tri_centers_y.push((pa[1] + pb[1] + pc[1]) / 3.0);
        self.tri_is_sliver.push(is_sliver);

        self.union(wa, wb);
        self.union(wb, wc);
        self.tri_comp_root.push(wa);
        self.tri_area.push(area);

        let verts = [(wa, pa), (wb, pb), (wc, pc)];
        for (i, j) in [(0, 1), (1, 2), (2, 0)] {
            let (u, pu) = verts[i];
            let (v, pv) = verts[j];
            let (ek, dir) = if u < v {
                ((u, v), 1i32)
            } else {
                ((v, u), -1i32)
            };
            let mid_y = 0.5 * (pu[1] + pv[1]);
            let entry = self.edges.entry(ek).or_insert(HygieneEdge {
                count: 0,
                bal: 0,
                normals: [[0.0; 3]; 2],
                mid_y,
            });
            if entry.count < 2 {
                entry.normals[entry.count] = fnrm;
            }
            entry.count += 1;
            entry.bal += dir;
        }
    }
}

struct UvPrimStats {
    valid_tris: usize,
    zero_uv_tris: usize,
    aniso_gt8: usize,
    max_aniso: f64,
    ratios: Vec<f64>,
    uv_nodes: BTreeMap<([i64; 3], i64, i64), usize>,
    uv_parent: Vec<usize>,
    tri_uv_rep: Vec<usize>,
}

impl Default for UvPrimStats {
    fn default() -> Self {
        Self {
            valid_tris: 0,
            zero_uv_tris: 0,
            aniso_gt8: 0,
            max_aniso: 1.0,
            ratios: Vec::new(),
            uv_nodes: BTreeMap::new(),
            uv_parent: Vec::new(),
            tri_uv_rep: Vec::new(),
        }
    }
}

impl UvPrimStats {
    fn node(&mut self, p: [f64; 3], uv: [f64; 2]) -> usize {
        let pk = [
            (p[0] * WELD_SCALE).round() as i64,
            (p[1] * WELD_SCALE).round() as i64,
            (p[2] * WELD_SCALE).round() as i64,
        ];
        let uk = (uv[0].clamp(-8.0, 8.0) * 65536.0).round() as i64;
        let vk = (uv[1].clamp(-8.0, 8.0) * 65536.0).round() as i64;
        let key = (pk, uk, vk);
        if let Some(&id) = self.uv_nodes.get(&key) {
            return id;
        }
        let id = self.uv_parent.len();
        self.uv_parent.push(id);
        self.uv_nodes.insert(key, id);
        id
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.uv_parent[x] != x {
            self.uv_parent[x] = self.uv_parent[self.uv_parent[x]];
            x = self.uv_parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.uv_parent[ra] = rb;
        }
    }

    fn push_tri(
        &mut self,
        pa: [f64; 3],
        pb: [f64; 3],
        pc: [f64; 3],
        ua: [f64; 2],
        ub: [f64; 2],
        uc: [f64; 2],
    ) {
        let area3 = 0.5 * len(cross(sub(pb, pa), sub(pc, pa)));
        if !area3.is_finite() || area3 <= 0.5 * MIN_AREA2 {
            return;
        }
        self.valid_tris += 1;

        let na = self.node(pa, ua);
        let nb = self.node(pb, ub);
        let nc = self.node(pc, uc);
        self.union(na, nb);
        self.union(nb, nc);
        self.tri_uv_rep.push(na);

        let d1 = [ub[0] - ua[0], ub[1] - ua[1]];
        let d2 = [uc[0] - ua[0], uc[1] - ua[1]];
        let uarea = 0.5 * (d1[0] * d2[1] - d1[1] * d2[0]).abs();
        if uarea <= 1e-10 {
            self.zero_uv_tris += 1;
            return;
        }
        self.ratios.push(uarea / area3);

        let l0 = len(sub(pb, pa));
        let l1 = len(sub(pc, pb));
        let l2 = len(sub(pa, pc));
        let u0 = ((ub[0] - ua[0]).hypot(ub[1] - ua[1])).max(1e-12);
        let u1 = ((uc[0] - ub[0]).hypot(uc[1] - ub[1])).max(1e-12);
        let u2 = ((ua[0] - uc[0]).hypot(ua[1] - uc[1])).max(1e-12);
        if l0 > 1e-9 && l1 > 1e-9 && l2 > 1e-9 {
            let r0 = u0 / l0;
            let r1 = u1 / l1;
            let r2 = u2 / l2;
            let rmax = r0.max(r1).max(r2);
            let rmin = r0.min(r1).min(r2).max(1e-9);
            let aniso = rmax / rmin;
            if aniso > self.max_aniso {
                self.max_aniso = aniso;
            }
            if aniso > 8.0 {
                self.aniso_gt8 += 1;
            }
        }
    }

    fn chart_count(&mut self) -> usize {
        let mut roots: BTreeMap<usize, usize> = BTreeMap::new();
        for i in 0..self.tri_uv_rep.len() {
            let rep = self.tri_uv_rep[i];
            let r = self.find(rep);
            *roots.entry(r).or_insert(0) += 1;
        }
        roots.len()
    }
}

/// Warn-level mesh-hygiene and character-conversion checks (`M_*`):
/// - `M_DEGENERATE_TRI`: duplicate-vertex or zero-area triangles
/// - `M_NON_MANIFOLD`: welded edges shared by > 2 faces
/// - `M_INCONSISTENT_WINDING`: adjacent 2-face manifold edges with conflicting half-edge winding
/// - `M_OPEN_EDGE`: open boundary edges on a mostly-closed body/prop mesh
/// - `M_NECK_CRACK`: open boundary crack in the neck band (72%..87% H) of a humanoid body mesh
/// - `M_SPECK`: tiny disconnected islands (< 0.5% of mesh surface area)
/// - `M_SLIVER`: needle triangles with minimum interior angle < 8 deg
/// - `M_FOLD`: acute self-fold creases (adjacent manifold faces folded > 98.6 deg / dihedral < 81.4 deg)
/// - `M_UV_SHRED`: severely stretched, collapsed, or hyper-fragmented UVs
pub fn check_mesh_hygiene(json: &Value, bin: &[u8]) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let meshes = arr(json, "meshes");
    let nodes = arr(json, "nodes");

    // Pre-collect welded boundary edges across all meshes so clean node-split seams
    // (e.g. *_BodyFace meeting *_Hair along coincident vertices) are not misreported
    // as open holes or neck cracks.
    let mut global_edge_counts: BTreeMap<([i64; 3], [i64; 3]), usize> = BTreeMap::new();
    let quant = |p: [f64; 3]| -> [i64; 3] {
        [
            (p[0] * WELD_SCALE).round() as i64,
            (p[1] * WELD_SCALE).round() as i64,
            (p[2] * WELD_SCALE).round() as i64,
        ]
    };
    if meshes.len() > 1 {
        for mesh in meshes {
            for prim in arr(mesh, "primitives") {
                let mode = prim.get("mode").and_then(Value::as_u64).unwrap_or(4);
                if mode != 4 && mode != 5 {
                    continue;
                }
                let Some(pos_i) = prim
                    .get("attributes")
                    .and_then(|a| a.get("POSITION"))
                    .and_then(as_idx)
                else {
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
                    Some(ii) if !is_sparse(json, ii) => {
                        weights::layout_of(json, bin.len(), ii, "indices")
                            .ok()
                            .filter(|l| {
                                l.width == 1
                                    && matches!(
                                        l.component,
                                        Component::U8 | Component::U16 | Component::U32
                                    )
                            })
                    }
                    _ => continue,
                };
                let n_idx = idx_lay.map_or(pos.count, |l| l.count);
                let read_idx = |k: usize| -> Option<usize> {
                    match &idx_lay {
                        Some(lay) => usize::try_from(lay.read_uint(bin, k, 0)?).ok(),
                        None => Some(k),
                    }
                };
                let mut add_tri = |a: usize, b: usize, c: usize| {
                    let (Some(pa), Some(pb), Some(pc)) = (
                        read_vec3(bin, &pos, a),
                        read_vec3(bin, &pos, b),
                        read_vec3(bin, &pos, c),
                    ) else {
                        return;
                    };
                    let (ka, kb, kc) = (quant(pa), quant(pb), quant(pc));
                    if ka == kb || kb == kc || kc == ka {
                        return;
                    }
                    for (u, v) in [(ka, kb), (kb, kc), (kc, ka)] {
                        let ek = if u < v { (u, v) } else { (v, u) };
                        *global_edge_counts.entry(ek).or_insert(0) += 1;
                    }
                };
                if mode == 4 {
                    let mut k = 0usize;
                    while k + 2 < n_idx {
                        if let (Some(a), Some(b), Some(c)) =
                            (read_idx(k), read_idx(k + 1), read_idx(k + 2))
                        {
                            if a < pos.count && b < pos.count && c < pos.count {
                                add_tri(a, b, c);
                            }
                        }
                        k += 3;
                    }
                } else {
                    for k in 0..n_idx.saturating_sub(2) {
                        if let (Some(a), Some(b), Some(c)) =
                            (read_idx(k), read_idx(k + 1), read_idx(k + 2))
                        {
                            if a < pos.count && b < pos.count && c < pos.count {
                                add_tri(a, b, c);
                            }
                        }
                    }
                }
            }
        }
    }

    // Track whole-scene vertical bounds across all meshes so split BodyFace + Hair
    // characters compute neck band (72%..87% H) from full character height, not BodyFace alone.
    let mut scene_min_y = f64::INFINITY;
    let mut scene_max_y = f64::NEG_INFINITY;
    for mesh in meshes {
        for prim in arr(mesh, "primitives") {
            if let Some(pos_i) = prim
                .get("attributes")
                .and_then(|a| a.get("POSITION"))
                .and_then(as_idx)
            {
                if let Some(acc) = arr(json, "accessors").get(pos_i) {
                    if let (Some(mn), Some(mx)) = (
                        acc.get("min").and_then(Value::as_array),
                        acc.get("max").and_then(Value::as_array),
                    ) {
                        if mn.len() >= 2 && mx.len() >= 2 {
                            if let (Some(y0), Some(y1)) = (mn[1].as_f64(), mx[1].as_f64()) {
                                scene_min_y = scene_min_y.min(y0);
                                scene_max_y = scene_max_y.max(y1);
                            }
                        }
                    }
                }
            }
        }
    }

    for (mi, mesh) in meshes.iter().enumerate() {
        let mtag = mesh_tag(meshes, mi);
        let node_open_sheet = nodes.iter().any(|n| {
            n.get("mesh").and_then(as_idx) == Some(mi)
                && n.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(is_intentional_open_sheet)
        });
        let open_sheet_name = is_intentional_open_sheet(&mtag) || node_open_sheet;
        let mut hb = HygieneBuilder::default();

        for (pi, prim) in arr(mesh, "primitives").iter().enumerate() {
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

            let uv_lay: Option<Layout> = match attrs.get("TEXCOORD_0").and_then(as_idx) {
                Some(ui) if !is_sparse(json, ui) => {
                    match weights::layout_of(json, bin.len(), ui, "TEXCOORD_0") {
                        Ok(l)
                            if l.component == Component::F32
                                && l.width == 2
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

            let mut uv_stats = uv_lay.as_ref().map(|_| UvPrimStats::default());
            let mut feed = |a: usize, b: usize, c: usize| {
                let (Some(pa), Some(pb), Some(pc)) = (
                    read_vec3(bin, &pos, a),
                    read_vec3(bin, &pos, b),
                    read_vec3(bin, &pos, c),
                ) else {
                    return;
                };
                hb.push_tri(pa, pb, pc);
                if let (Some(ul), Some(us)) = (uv_lay.as_ref(), uv_stats.as_mut()) {
                    if let (Some(ua), Some(ub), Some(uc)) = (
                        read_vec2(bin, ul, a),
                        read_vec2(bin, ul, b),
                        read_vec2(bin, ul, c),
                    ) {
                        us.push_tri(pa, pb, pc, ua, ub, uc);
                    }
                }
            };

            if mode == 4 {
                let mut k = 0usize;
                while k + 2 < n_idx {
                    if let (Some(a), Some(b), Some(c)) =
                        (read_idx(k), read_idx(k + 1), read_idx(k + 2))
                    {
                        if a < pos.count && b < pos.count && c < pos.count {
                            feed(a, b, c);
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
                                feed(a, b, c);
                            } else {
                                feed(a, c, b);
                            }
                        }
                    }
                }
            }

            // Evaluate per-primitive UV shredding / stretch distortion.
            if let Some(mut us) = uv_stats {
                if us.valid_tris >= 12 {
                    let aniso_pct = (us.aniso_gt8 as f64) * 100.0 / (us.valid_tris as f64);
                    let zero_pct = (us.zero_uv_tris as f64) * 100.0 / (us.valid_tris as f64);
                    let mut spread = 1.0f64;
                    if us.ratios.len() >= 50 {
                        us.ratios.sort_by(|a, b| a.total_cmp(b));
                        let p5 = us.ratios[(us.ratios.len() * 5) / 100].max(1e-12);
                        let p95 =
                            us.ratios[((us.ratios.len() * 95) / 100).min(us.ratios.len() - 1)];
                        spread = p95 / p5;
                    }
                    let charts = us.chart_count();
                    let avg_tris_per_chart = (us.valid_tris as f64) / (charts.max(1) as f64);

                    let bad_aniso = us.aniso_gt8 >= 5 && aniso_pct >= 1.5;
                    let bad_spread = us.ratios.len() >= 50 && spread > 25.0;
                    // Only flag zero-UV collapse if the mesh actually uses non-zero UVs elsewhere
                    let has_real_uv = us.ratios.len() >= us.valid_tris / 4;
                    let bad_zero = has_real_uv && us.zero_uv_tris >= 5 && zero_pct >= 2.0;
                    // Only flag chart fragmentation when paired with real UV distortion or extreme
                    // triangle-soup shattering (avoids false alarms on clean preserved head/hair atlases).
                    let bad_shred_charts =
                        (us.valid_tris >= 500 && charts > 300 && avg_tris_per_chart < 8.0)
                            || (us.valid_tris >= 500
                                && charts > 200
                                && avg_tris_per_chart < 35.0
                                && (spread > 8.0 || aniso_pct >= 0.5));

                    if bad_aniso || bad_spread || bad_zero || bad_shred_charts {
                        out.push((
                            "M_UV_SHRED",
                            format!(
                                "{mtag} prim {pi}: distorted/shredded UVs \
                                 (aniso>8x: {} [{aniso_pct:.1}%], spread p95/p5: {spread:.1}x, \
                                 zero-UV tris: {}, charts: {charts} across {} tris)",
                                us.aniso_gt8, us.zero_uv_tris, us.valid_tris
                            ),
                        ));
                    }
                }
            }
        }

        if hb.valid_tris == 0 && hb.welded.len() <= 1 {
            // All-zero placeholder geometry buffer (no distinct vertex positions): skip hygiene.
            continue;
        }

        if hb.degenerate_tris > 0 {
            out.push((
                "M_DEGENERATE_TRI",
                format!(
                    "{mtag}: {} degenerate triangle{} (zero area or duplicate vertices)",
                    hb.degenerate_tris,
                    if hb.degenerate_tris == 1 { "" } else { "s" }
                ),
            ));
        }

        if hb.valid_tris == 0 {
            continue;
        }

        let eff_min_y = if scene_min_y.is_finite() {
            scene_min_y.min(hb.min_y)
        } else {
            hb.min_y
        };
        let eff_max_y = if scene_max_y.is_finite() {
            scene_max_y.max(hb.max_y)
        } else {
            hb.max_y
        };
        let height = (eff_max_y - eff_min_y).max(0.0);
        let width_x = (hb.max_x - hb.min_x).max(0.0);
        let depth_z = (hb.max_z - hb.min_z).max(0.0);
        let is_humanoid = height > 0.3 && height > 1.2 * depth_z && height > 0.8 * width_x;
        let neck_lo = eff_min_y + 0.72 * height;
        let neck_hi = eff_min_y + 0.87 * height;

        if is_humanoid {
            for (i, &is_s) in hb.tri_is_sliver.iter().enumerate() {
                if is_s {
                    let cy = hb.tri_centers_y[i];
                    if cy >= neck_lo && cy <= neck_hi {
                        hb.neck_sliver_tris += 1;
                    }
                }
            }
        }

        // Reverse lookup from welded vertex id to quantized coordinate key for cross-mesh seam check.
        let mut id_to_key: Vec<[i64; 3]> = vec![[0; 3]; hb.parent.len()];
        if meshes.len() > 1 {
            for (&k, &id) in &hb.welded {
                if id < id_to_key.len() {
                    id_to_key[id] = k;
                }
            }
        }

        let total_edges = hb.edges.len();
        let mut open_edges = 0usize;
        let mut neck_open_edges = 0usize;
        let mut non_manifold_edges = 0usize;
        let mut inconsistent_winding_edges = 0usize;
        let mut self_fold_edges = 0usize;

        for (&(u, v), edge) in &hb.edges {
            if edge.count == 1 {
                let stitched_by_sibling = if meshes.len() > 1 {
                    let ku = id_to_key[u];
                    let kv = id_to_key[v];
                    let gk = if ku < kv { (ku, kv) } else { (kv, ku) };
                    global_edge_counts.get(&gk).copied().unwrap_or(1) >= 2
                } else {
                    false
                };
                if !stitched_by_sibling {
                    open_edges += 1;
                    if is_humanoid && edge.mid_y >= neck_lo && edge.mid_y <= neck_hi {
                        neck_open_edges += 1;
                    }
                }
            } else if edge.count == 2 {
                if edge.bal != 0 {
                    inconsistent_winding_edges += 1;
                } else {
                    let d = dot(edge.normals[0], edge.normals[1]);
                    if d < SELF_FOLD_MAX_DOT {
                        self_fold_edges += 1;
                    }
                }
            } else if edge.count > 2 {
                non_manifold_edges += 1;
            }
        }

        if non_manifold_edges > 0 {
            out.push((
                "M_NON_MANIFOLD",
                format!(
                    "{mtag}: {non_manifold_edges} non-manifold edge{} (> 2 faces)",
                    if non_manifold_edges == 1 { "" } else { "s" }
                ),
            ));
        }

        if inconsistent_winding_edges > 0 {
            out.push((
                "M_INCONSISTENT_WINDING",
                format!(
                    "{mtag}: {inconsistent_winding_edges} edge{} with conflicting adjacent face winding",
                    if inconsistent_winding_edges == 1 { "" } else { "s" }
                ),
            ));
        }

        // Open edges on mostly-closed meshes (ignore tiny <12-tri test snippets and
        // intentional open cards/sheets/planes where >25% of edges are boundary).
        let mostly_closed = hb.valid_tris >= 12 && total_edges > 0 && open_edges * 4 <= total_edges;
        if !open_sheet_name && mostly_closed && open_edges > 0 {
            out.push((
                "M_OPEN_EDGE",
                format!(
                    "{mtag}: {open_edges} open boundary edge{} on mostly-closed mesh ({} tris)",
                    if open_edges == 1 { "" } else { "s" },
                    hb.valid_tris
                ),
            ));
            if is_humanoid && neck_open_edges >= 2 {
                out.push((
                    "M_NECK_CRACK",
                    format!(
                        "{mtag}: {neck_open_edges} open boundary edges in neck band \
                         ({neck_lo:.3}..{neck_hi:.3}m)"
                    ),
                ));
            }
        }

        // Connected-component speck check (< 0.5% of total area on multi-island meshes).
        let mut comp_area: BTreeMap<usize, (f64, usize)> = BTreeMap::new();
        let mut total_area = 0.0f64;
        for i in 0..hb.tri_comp_root.len() {
            let root = hb.find(hb.tri_comp_root[i]);
            let a = hb.tri_area[i];
            total_area += a;
            let entry = comp_area.entry(root).or_insert((0.0, 0));
            entry.0 += a;
            entry.1 += 1;
        }
        if comp_area.len() >= 2 && total_area > 0.0 {
            let max_speck_tris = (hb.valid_tris / 200).max(32);
            let mut speck_islands = 0usize;
            let mut speck_tris = 0usize;
            for &(ca, ct) in comp_area.values() {
                if ca < 0.005 * total_area && ct <= max_speck_tris {
                    speck_islands += 1;
                    speck_tris += ct;
                }
            }
            if speck_islands > 0 {
                out.push((
                    "M_SPECK",
                    format!(
                        "{mtag}: {speck_islands} tiny disconnected island{} (< 0.5% surface area, \
                         {speck_tris} tris total)",
                        if speck_islands == 1 { "" } else { "s" }
                    ),
                ));
            }
        }

        // Slivers (< 8 deg min interior angle).
        let sliver_pct = (hb.sliver_tris as f64) * 100.0 / (hb.valid_tris as f64);
        let bad_sliver = (hb.sliver_tris >= 3 && (sliver_pct >= 0.5 || hb.sliver_tris >= 25))
            || (is_humanoid && hb.neck_sliver_tris >= 5);
        if bad_sliver {
            out.push((
                "M_SLIVER",
                format!(
                    "{mtag}: {} sliver triangles (< {SLIVER_MIN_ANGLE_DEG:.0} deg, {sliver_pct:.2}%, \
                     neck={}, worst {:.2} deg)",
                    hb.sliver_tris, hb.neck_sliver_tris, hb.worst_sliver_deg
                ),
            ));
        }

        // Self-folds (acute fold-back creases on manifold edges).
        let fold_pct = if total_edges > 0 {
            (self_fold_edges as f64) * 100.0 / (total_edges as f64)
        } else {
            0.0
        };
        if self_fold_edges >= 3 && (fold_pct >= 0.2 || self_fold_edges >= 15) {
            out.push((
                "M_FOLD",
                format!(
                    "{mtag}: {self_fold_edges} acute self-fold manifold edges ({fold_pct:.2}%, \
                     dihedral < 60 deg)"
                ),
            ));
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

    #[test]
    fn small_prop_requires_at_least_two_strictly_inverted_triangles() {
        let make = |n_flipped: usize| {
            let mut pos = Vec::new();
            let mut nrm = Vec::new();
            for i in 0..400 {
                let x = i as f32;
                pos.extend_from_slice(&[[x, 0.0, 0.0], [x + 1.0, 0.0, 0.0], [x, 1.0, 0.0]]);
                let nz = if i < n_flipped { -1.0 } else { 1.0 };
                nrm.extend_from_slice(&[[0.0, 0.0, nz]; 3]);
            }
            build_mesh_glb(&pos, &nrm, None, Some(4), false, None)
        };
        // 1/400 = 0.25% >= 0.2%, but < 2 inverted triangles -> avoids single thin-card false alarm.
        assert!(!codes(&make(1)).contains(&"M_FLIPPED_NORMAL"));
        // 2/400 = 0.5% and >= 2 inverted triangles -> flags cleanly.
        assert!(codes(&make(2)).contains(&"M_FLIPPED_NORMAL"));
    }

    #[test]
    fn multi_primitive_inside_out_closed_shell_flags() {
        // Split a single inside-out cube across two material primitives (3 faces each):
        // each primitive alone is an open half-box, but together they form a closed shell.
        let (pos, nrm, idx) = unit_cube(true);
        let mut bin = Vec::new();
        for p in &pos {
            for &x in p {
                bin.extend_from_slice(&x.to_le_bytes());
            }
        }
        let pos_len = bin.len();
        let nrm_off = bin.len();
        for n in &nrm {
            for &x in n {
                bin.extend_from_slice(&x.to_le_bytes());
            }
        }
        let nrm_len = bin.len() - nrm_off;
        let split = idx.len() / 2;
        let idx0_off = bin.len();
        for &i in &idx[..split] {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        let idx0_len = bin.len() - idx0_off;
        let idx1_off = bin.len();
        for &i in &idx[split..] {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        let idx1_len = bin.len() - idx1_off;
        let doc = json!({
            "asset": {"version": "2.0"},
            "scenes": [{"nodes": [0]}],
            "nodes": [{"name": "SplitProp", "mesh": 0}],
            "materials": [{"name": "MatA"}, {"name": "MatB"}],
            "meshes": [{
                "name": "MultiMatProp",
                "primitives": [
                    {"attributes": {"POSITION": 0, "NORMAL": 1}, "indices": 2, "material": 0},
                    {"attributes": {"POSITION": 0, "NORMAL": 1}, "indices": 3, "material": 1},
                ]
            }],
            "buffers": [{"byteLength": bin.len()}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": pos_len},
                {"buffer": 0, "byteOffset": nrm_off, "byteLength": nrm_len},
                {"buffer": 0, "byteOffset": idx0_off, "byteLength": idx0_len},
                {"buffer": 0, "byteOffset": idx1_off, "byteLength": idx1_len},
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": pos.len(), "type": "VEC3"},
                {"bufferView": 1, "componentType": 5126, "count": nrm.len(), "type": "VEC3"},
                {"bufferView": 2, "componentType": 5123, "count": split, "type": "SCALAR"},
                {"bufferView": 3, "componentType": 5123, "count": idx.len() - split, "type": "SCALAR"},
            ],
        });
        let glb = pack_bin(&doc, &bin);
        let r = checks::check_glb_with_class(&glb, None, Some(AssetClass::Prop));
        let d = r
            .diags
            .iter()
            .find(|d| d.code == "M_FLIPPED_NORMAL")
            .expect("multi-material inside-out prop must flag M_FLIPPED_NORMAL");
        assert!(d.detail.contains("12 of 12 triangles"), "{}", d.detail);
    }

    #[test]
    fn intentional_skybox_and_outline_inward_shells_skip_signed_volume() {
        let (pos, nrm, idx) = unit_cube(true);
        for mesh_name in ["World_Skybox", "Character_Outline", "Body_Inverted_Hull"] {
            let mut bin = Vec::new();
            for p in &pos {
                for &x in p {
                    bin.extend_from_slice(&x.to_le_bytes());
                }
            }
            let pos_len = bin.len();
            let nrm_off = bin.len();
            for n in &nrm {
                for &x in n {
                    bin.extend_from_slice(&x.to_le_bytes());
                }
            }
            let nrm_len = bin.len() - nrm_off;
            let idx_off = bin.len();
            for &i in &idx {
                bin.extend_from_slice(&i.to_le_bytes());
            }
            let idx_len = bin.len() - idx_off;
            let doc = json!({
                "asset": {"version": "2.0"},
                "scenes": [{"nodes": [0]}],
                "nodes": [{"name": mesh_name, "mesh": 0}],
                "materials": [{"name": "Mat"}],
                "meshes": [{
                    "name": mesh_name,
                    "primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1}, "indices": 2, "material": 0}]
                }],
                "buffers": [{"byteLength": bin.len()}],
                "bufferViews": [
                    {"buffer": 0, "byteOffset": 0, "byteLength": pos_len},
                    {"buffer": 0, "byteOffset": nrm_off, "byteLength": nrm_len},
                    {"buffer": 0, "byteOffset": idx_off, "byteLength": idx_len},
                ],
                "accessors": [
                    {"bufferView": 0, "componentType": 5126, "count": pos.len(), "type": "VEC3"},
                    {"bufferView": 1, "componentType": 5126, "count": nrm.len(), "type": "VEC3"},
                    {"bufferView": 2, "componentType": 5123, "count": idx.len(), "type": "SCALAR"},
                ],
            });
            let glb = pack_bin(&doc, &bin);
            assert!(
                !codes(&glb).contains(&"M_FLIPPED_NORMAL"),
                "{mesh_name} should skip inward shell check"
            );
        }
    }

    fn warn_codes(bytes: &[u8]) -> Vec<&'static str> {
        let r = checks::check_glb_with_class(bytes, None, Some(AssetClass::Prop));
        r.warnings().iter().map(|d| d.code).collect()
    }

    fn strict_codes(bytes: &[u8]) -> Vec<&'static str> {
        let opts = checks::CheckOpts {
            strict_mesh: true,
            ..Default::default()
        };
        let r = checks::check_glb_with_opts(bytes, None, Some(AssetClass::Prop), &opts);
        r.diags.iter().map(|d| d.code).collect()
    }

    fn build_uv_mesh_glb(
        name: &str,
        positions: &[[f32; 3]],
        uvs: Option<&[[f32; 2]]>,
        indices: &[u16],
    ) -> Vec<u8> {
        let mut bin = Vec::new();
        for p in positions {
            for &x in p {
                bin.extend_from_slice(&x.to_le_bytes());
            }
        }
        let pos_len = bin.len();
        let mut views = vec![json!({"buffer": 0, "byteOffset": 0, "byteLength": pos_len})];
        let mut accs = vec![
            json!({"bufferView": 0, "componentType": 5126, "count": positions.len(), "type": "VEC3"}),
        ];
        let mut attrs = json!({"POSITION": 0});
        if let Some(uv) = uvs {
            let uv_off = bin.len();
            for u in uv {
                for &x in u {
                    bin.extend_from_slice(&x.to_le_bytes());
                }
            }
            let uv_len = bin.len() - uv_off;
            let vi = views.len();
            views.push(json!({"buffer": 0, "byteOffset": uv_off, "byteLength": uv_len}));
            accs.push(
                json!({"bufferView": vi, "componentType": 5126, "count": uv.len(), "type": "VEC2"}),
            );
            attrs["TEXCOORD_0"] = json!(vi);
        }
        let idx_off = bin.len();
        for &i in indices {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        let idx_len = bin.len() - idx_off;
        let iv = views.len();
        views.push(json!({"buffer": 0, "byteOffset": idx_off, "byteLength": idx_len}));
        accs.push(json!({"bufferView": iv, "componentType": 5123, "count": indices.len(), "type": "SCALAR"}));
        let doc = json!({
            "asset": {"version": "2.0"},
            "scenes": [{"nodes": [0]}],
            "nodes": [{"name": name, "mesh": 0}],
            "meshes": [{"name": name, "primitives": [{"attributes": attrs, "indices": iv}]}],
            "buffers": [{"byteLength": bin.len()}],
            "bufferViews": views,
            "accessors": accs,
        });
        pack_bin(&doc, &bin)
    }

    fn closed_capsule_humanoid(open_neck_crack: bool) -> Vec<u8> {
        // Closed 1.75m tall humanoid-proportioned cylinder+caps (glTF Y-up).
        let segs = 16usize;
        let rings = 14usize;
        let mut pos: Vec<[f32; 3]> = Vec::new();
        let mut idx: Vec<u16> = Vec::new();
        pos.push([0.0, 0.0, 0.0]); // bottom center = 0
        for r in 0..=rings {
            let t = (r as f32) / (rings as f32);
            let y = 0.02 + t * 1.71;
            let rad = if (0.75..=0.85).contains(&t) {
                0.06
            } else {
                0.14
            };
            for s in 0..segs {
                let a = 2.0 * std::f32::consts::PI * (s as f32) / (segs as f32);
                pos.push([rad * a.cos(), y, rad * a.sin()]);
            }
        }
        let top_c = pos.len() as u16;
        pos.push([0.0, 1.75, 0.0]);
        for s in 0..segs {
            let a = 1 + s;
            let b = 1 + (s + 1) % segs;
            idx.extend_from_slice(&[0, a as u16, b as u16]);
        }
        for r in 0..rings {
            let row0 = 1 + r * segs;
            let row1 = 1 + (r + 1) * segs;
            // Ring 11 sits around y ~= 1.38m (~79% height = neck band)
            for s in 0..segs {
                if open_neck_crack && r == 11 && s < 4 {
                    continue;
                }
                let s1 = (s + 1) % segs;
                let a = (row0 + s) as u16;
                let b = (row0 + s1) as u16;
                let c = (row1 + s1) as u16;
                let d = (row1 + s) as u16;
                idx.extend_from_slice(&[a, c, b, a, d, c]);
            }
        }
        let last = 1 + rings * segs;
        for s in 0..segs {
            let a = (last + s) as u16;
            let b = (last + (s + 1) % segs) as u16;
            idx.extend_from_slice(&[a, top_c, b]);
        }
        build_uv_mesh_glb("Hero_BodyFace", &pos, None, &idx)
    }

    #[test]
    fn clean_closed_cube_passes_all_hygiene_checks() {
        let (pos, nrm, idx) = unit_cube(false);
        let glb = build_mesh_glb(&pos, &nrm, Some(&idx), None, false, None);
        assert!(warn_codes(&glb).is_empty(), "got: {:?}", warn_codes(&glb));
        assert!(
            strict_codes(&glb).is_empty(),
            "got: {:?}",
            strict_codes(&glb)
        );
    }

    #[test]
    fn planted_open_edge_and_neck_crack_detected() {
        let clean = closed_capsule_humanoid(false);
        assert!(
            warn_codes(&clean).is_empty(),
            "clean humanoid warned: {:?}",
            warn_codes(&clean)
        );

        let cracked = closed_capsule_humanoid(true);
        let w = warn_codes(&cracked);
        assert!(
            w.contains(&"M_OPEN_EDGE"),
            "expected M_OPEN_EDGE, got {w:?}"
        );
        assert!(
            w.contains(&"M_NECK_CRACK"),
            "expected M_NECK_CRACK, got {w:?}"
        );
        assert!(strict_codes(&cracked).contains(&"M_NECK_CRACK"));
    }

    #[test]
    fn planted_non_manifold_edge_detected() {
        let (mut pos, _, mut idx) = unit_cube(false);
        // Add a 3rd fin triangle attached to an existing cube edge (0, 1).
        let tip = pos.len() as u16;
        pos.push([0.5, 0.5, 2.0]);
        idx.extend_from_slice(&[0, 1, tip]);
        let glb = build_uv_mesh_glb("BoxFin", &pos, None, &idx);
        assert!(warn_codes(&glb).contains(&"M_NON_MANIFOLD"));
        assert!(strict_codes(&glb).contains(&"M_NON_MANIFOLD"));
    }

    #[test]
    fn planted_degenerate_triangle_detected() {
        let (pos, _, mut idx) = unit_cube(false);
        idx.extend_from_slice(&[0, 0, 1]);
        let glb = build_uv_mesh_glb("BoxDeg", &pos, None, &idx);
        assert!(warn_codes(&glb).contains(&"M_DEGENERATE_TRI"));
        assert!(strict_codes(&glb).contains(&"M_DEGENERATE_TRI"));
    }

    #[test]
    fn planted_inconsistent_winding_detected() {
        let (pos, _, mut idx) = unit_cube(false);
        // Flip one triangle's winding so it disagrees with its neighbors.
        idx.swap(1, 2);
        let glb = build_uv_mesh_glb("BoxWind", &pos, None, &idx);
        assert!(warn_codes(&glb).contains(&"M_INCONSISTENT_WINDING"));
    }

    #[test]
    fn planted_speck_island_detected() {
        let (mut pos, _, mut idx) = unit_cube(false);
        let b = pos.len() as u16;
        // Tiny 0.005m floating speck tetrahedron (< 0.5% of cube area 6.0).
        pos.extend_from_slice(&[
            [3.0, 3.0, 3.0],
            [3.005, 3.0, 3.0],
            [3.0, 3.005, 3.0],
            [3.0, 3.0, 3.005],
        ]);
        idx.extend_from_slice(&[
            b,
            b + 2,
            b + 1,
            b,
            b + 1,
            b + 3,
            b + 1,
            b + 2,
            b + 3,
            b + 2,
            b,
            b + 3,
        ]);
        let glb = build_uv_mesh_glb("BoxSpeck", &pos, None, &idx);
        assert!(warn_codes(&glb).contains(&"M_SPECK"));
    }

    #[test]
    fn planted_sliver_fan_detected() {
        let (mut pos, _, mut idx) = unit_cube(false);
        // Add a fan of 6 ultra-thin < 4 deg needle triangles.
        let base = pos.len() as u16;
        pos.push([2.0, 0.0, 0.0]);
        for i in 0..7 {
            pos.push([3.0, 0.015 * (i as f32), 0.0]);
        }
        for i in 0..6u16 {
            idx.extend_from_slice(&[base, base + 1 + i, base + 2 + i]);
        }
        let glb = build_uv_mesh_glb("SliverMesh", &pos, None, &idx);
        assert!(warn_codes(&glb).contains(&"M_SLIVER"));
        assert!(strict_codes(&glb).contains(&"M_SLIVER"));
    }

    #[test]
    fn planted_acute_self_fold_detected() {
        // Build a flat sheet with 4 triangles folded sharply >140 deg back on themselves.
        let mut pos: Vec<[f32; 3]> = Vec::new();
        let mut idx: Vec<u16> = Vec::new();
        for i in 0..4 {
            let x0 = i as f32;
            let b = pos.len() as u16;
            // Two adjacent triangles sharing edge (b, b+1):
            // tri0 lies in z=0 (+Y), tri1 folds almost completely back over tri0 (z=0.05, +Y)
            // so their outward normals point almost opposite (dot < -0.9).
            pos.push([x0, 0.0, 0.0]);
            pos.push([x0 + 0.8, 0.0, 0.0]);
            pos.push([x0 + 0.4, 1.0, 0.0]);
            pos.push([x0 + 0.4, 0.95, 0.08]);
            idx.extend_from_slice(&[b, b + 1, b + 2, b + 1, b, b + 3]);
        }
        let glb = build_uv_mesh_glb("FoldedMesh", &pos, None, &idx);
        assert!(warn_codes(&glb).contains(&"M_FOLD"));
        assert!(strict_codes(&glb).contains(&"M_FOLD"));
    }

    #[test]
    fn planted_shredded_uvs_detected_and_clean_uvs_pass() {
        // 6x6 quad grid = 72 triangles.
        let n = 6usize;
        let mut pos: Vec<[f32; 3]> = Vec::new();
        let mut uv_clean: Vec<[f32; 2]> = Vec::new();
        let mut uv_bad: Vec<[f32; 2]> = Vec::new();
        let mut idx: Vec<u16> = Vec::new();
        for y in 0..=n {
            for x in 0..=n {
                let fx = (x as f32) / (n as f32);
                let fy = (y as f32) / (n as f32);
                pos.push([fx, fy, 0.0]);
                uv_clean.push([fx, fy]);
                // Plant extreme anisotropic stretch (>20x) on a strip of vertices
                let bad_u = if x == 2 { fx + 0.45 } else { fx * 0.05 };
                uv_bad.push([bad_u, fy]);
            }
        }
        for y in 0..n {
            for x in 0..n {
                let a = (y * (n + 1) + x) as u16;
                let b = a + 1;
                let c = a + (n + 1) as u16;
                let d = c + 1;
                idx.extend_from_slice(&[a, b, d, a, d, c]);
            }
        }
        let clean_glb = build_uv_mesh_glb("PlaneClean", &pos, Some(&uv_clean), &idx);
        assert!(!warn_codes(&clean_glb).contains(&"M_UV_SHRED"));

        let bad_glb = build_uv_mesh_glb("PlaneShred", &pos, Some(&uv_bad), &idx);
        assert!(warn_codes(&bad_glb).contains(&"M_UV_SHRED"));
        assert!(strict_codes(&bad_glb).contains(&"M_UV_SHRED"));
    }
}
