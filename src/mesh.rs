// SPDX-License-Identifier: MIT
//! Mesh-geometry checks (M_* layer).
//!
//! Catches inverted / flipped-normal triangles where exported vertex normals
//! oppose the geometric face normal (`(p1 - p0) x (p2 - p0)`). Unreadable,
//! non-triangle, or sparse primitives are skipped cleanly without panicking.

use crate::util::{arr, as_idx, mesh_tag};
use crate::weights;
use glbkit::accessor::{Component, Layout};
use serde_json::Value;

/// Minimum cross-product magnitude for a triangle to be considered non-degenerate.
const MIN_AREA2: f64 = 1e-12;
/// Minimum normal length to avoid dividing by zero on unset normals.
const MIN_NORM: f64 = 1e-8;
/// Per-vertex dot-product threshold below which a vertex normal strictly opposes
/// the geometric face normal (avoids grazing ~90° smooth-shading creases).
const STRICT_NEG_DOT: f64 = -0.1;

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
fn len(v: [f64; 3]) -> f64 {
    dot(v, v).sqrt()
}

#[derive(Default)]
struct NormalCounts {
    total: usize,
    flipped_avg: usize,
    flipped_all: usize,
}

#[inline]
fn check_triangle(
    bin: &[u8],
    pos: &Layout,
    nrm: &Layout,
    tri: [usize; 3],
    counts: &mut NormalCounts,
) {
    let [a, b, c] = tri;
    let (Some(pa), Some(pb), Some(pc)) = (
        read_vec3(bin, pos, a),
        read_vec3(bin, pos, b),
        read_vec3(bin, pos, c),
    ) else {
        return;
    };
    let u = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
    let v = [pc[0] - pa[0], pc[1] - pa[1], pc[2] - pa[2]];
    let face = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let fl = len(face);
    if !fl.is_finite() || fl <= MIN_AREA2 {
        return;
    }
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

/// Flag triangle primitives whose vertex normals systematically oppose their
/// geometric face normals (inverted winding or flipped custom normals).
///
/// A single smooth-shaded grazing crease can push 1-2 vertices slightly past
/// 90° without flipping the surface, so we require either:
/// - at least 0.5% of valid triangles have all 3 vertex normals strictly
///   opposing the face normal (`dot < -0.1`), or
/// - at least 2% of valid triangles (and >= 10) have opposing average normal
///   alongside at least 5 strictly inverted triangles.
pub fn check_mesh(json: &Value, bin: &[u8]) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let meshes = arr(json, "meshes");
    for (mi, mesh) in meshes.iter().enumerate() {
        let mtag = mesh_tag(meshes, mi);
        for (pi, prim) in arr(mesh, "primitives").iter().enumerate() {
            let mode = prim.get("mode").and_then(Value::as_u64).unwrap_or(4);
            if mode != 4 && mode != 5 {
                continue;
            }
            let attrs = match prim.get("attributes") {
                Some(a) => a,
                None => continue,
            };
            let (Some(pos_i), Some(nrm_i)) = (
                attrs.get("POSITION").and_then(as_idx),
                attrs.get("NORMAL").and_then(as_idx),
            ) else {
                continue;
            };
            if is_sparse(json, pos_i) || is_sparse(json, nrm_i) {
                continue;
            }
            let (Ok(pos), Ok(nrm)) = (
                weights::layout_of(json, bin.len(), pos_i, "POSITION"),
                weights::layout_of(json, bin.len(), nrm_i, "NORMAL"),
            ) else {
                continue;
            };
            if pos.component != Component::F32
                || pos.width != 3
                || nrm.component != Component::F32
                || nrm.width != 3
                || pos.count != nrm.count
                || pos.count < 3
            {
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

            let mut counts = NormalCounts::default();

            if mode == 4 {
                let mut k = 0usize;
                while k + 2 < n_idx {
                    if let (Some(a), Some(b), Some(c)) =
                        (read_idx(k), read_idx(k + 1), read_idx(k + 2))
                    {
                        if a < pos.count && b < pos.count && c < pos.count {
                            check_triangle(bin, &pos, &nrm, [a, b, c], &mut counts);
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
                            check_triangle(bin, &pos, &nrm, tri, &mut counts);
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
                flipped_all >= 1 && flipped_all.saturating_mul(1000) >= total.saturating_mul(5);
            let bad_avg = flipped_avg >= 10
                && flipped_avg.saturating_mul(100) >= total.saturating_mul(2)
                && flipped_all >= 5;
            if bad_all || bad_avg {
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
        let doc = json!({
            "asset": {"version": "2.0"},
            "scenes": [{"nodes": [0]}],
            "nodes": [{"name": "Mesh", "mesh": 0}],
            "materials": [{"name": "Mat", "doubleSided": double_sided}],
            "meshes": [{"name": "TestMesh", "primitives": [prim]}],
            "buffers": [{"byteLength": bin.len()}],
            "bufferViews": views,
            "accessors": accs,
        });
        pack_bin(&doc, &bin)
    }

    fn codes(bytes: &[u8]) -> Vec<&'static str> {
        let r = checks::check_glb_with_class(bytes, None, Some(AssetClass::Prop));
        r.diags.iter().map(|d| d.code).collect()
    }

    #[test]
    fn clean_triangles_pass() {
        // CCW triangle in XY plane has +Z geometric normal matching +Z vertex normals.
        let pos = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let nrm = [[0.0, 0.0, 1.0]; 3];
        let glb = build_mesh_glb(&pos, &nrm, Some(&[0, 1, 2]), None, false);
        assert!(!codes(&glb).contains(&"M_FLIPPED_NORMAL"));
    }

    #[test]
    fn flipped_winding_flags() {
        // CW triangle in XY plane has -Z geometric normal opposing +Z vertex normals.
        let pos = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let nrm = [[0.0, 0.0, 1.0]; 3];
        let glb = build_mesh_glb(&pos, &nrm, Some(&[0, 2, 1]), None, false);
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
        // Blender exports doubleSided=true by default; inverted normals must still be caught.
        let pos = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let nrm = [[0.0, 0.0, -1.0]; 3];
        let glb = build_mesh_glb(&pos, &nrm, None, Some(4), true);
        assert!(codes(&glb).contains(&"M_FLIPPED_NORMAL"));
    }

    #[test]
    fn triangle_strip_winding_respected() {
        // Valid 2-triangle quad strip in XY with +Z normals must pass.
        let pos = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
        ];
        let nrm = [[0.0, 0.0, 1.0]; 4];
        let clean = build_mesh_glb(&pos, &nrm, Some(&[0, 1, 2, 3]), Some(5), false);
        assert!(!codes(&clean).contains(&"M_FLIPPED_NORMAL"));

        // Negated normals on the same strip must fail.
        let bad_nrm = [[0.0, 0.0, -1.0]; 4];
        let bad = build_mesh_glb(&pos, &bad_nrm, Some(&[0, 1, 2, 3]), Some(5), false);
        assert!(codes(&bad).contains(&"M_FLIPPED_NORMAL"));
    }

    #[test]
    fn degenerate_triangles_ignored() {
        // Collinear zero-area triangle with opposing normals must not trigger.
        let pos = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]];
        let nrm = [[0.0, 0.0, -1.0]; 3];
        let glb = build_mesh_glb(&pos, &nrm, Some(&[0, 1, 2]), None, false);
        assert!(!codes(&glb).contains(&"M_FLIPPED_NORMAL"));
    }

    #[test]
    fn isolated_smooth_crease_in_large_mesh_passes() {
        // 300 clean triangles + 1 grazing crease triangle where 2 verts lean slightly
        // past 90° and 1 vert stays positive (common in valid smooth-shaded creases).
        let mut pos = Vec::new();
        let mut nrm = Vec::new();
        for i in 0..300 {
            let x = i as f32;
            pos.extend_from_slice(&[[x, 0.0, 0.0], [x + 1.0, 0.0, 0.0], [x, 1.0, 0.0]]);
            nrm.extend_from_slice(&[[0.0, 0.0, 1.0]; 3]);
        }
        // Add 1 grazing smooth crease triangle (avg_dot < 0, but vertex 0 has dot > 0)
        pos.extend_from_slice(&[[400.0, 0.0, 0.0], [401.0, 0.0, 0.0], [400.0, 1.0, 0.0]]);
        nrm.extend_from_slice(&[[0.0, 0.0, 0.2], [0.0, 0.9, -0.3], [0.0, 0.9, -0.3]]);
        let glb = build_mesh_glb(&pos, &nrm, None, Some(4), true);
        assert!(!codes(&glb).contains(&"M_FLIPPED_NORMAL"));
    }
}
