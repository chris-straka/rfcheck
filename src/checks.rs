// SPDX-License-Identifier: MIT
//! Rig-contract checks over a parsed GLB document.

use crate::bevy;
use crate::budget;
use crate::defects;
use crate::glb;
use crate::util::{arr, join_names, plural};
use crate::weights;
use serde_json::{json, Value};

pub struct Diag {
    pub code: &'static str,
    pub detail: String,
}

pub struct Report {
    pub diags: Vec<Diag>,
    /// Budget (P_*) warnings. None when no budget was requested, in
    /// which case the report (and its JSON shape) is exactly v0.3.0.
    pub warns: Option<Vec<Diag>>,
    pub summary: String,
    /// Asset class the file was checked as (`--class`). None for
    /// unclassed runs, whose output is unchanged.
    pub class: Option<budget::AssetClass>,
}

impl Report {
    /// The file could not be read at all (R_IO). Goes through the same
    /// serializer as every other report, so odd paths (Windows `\`,
    /// quotes) still yield valid JSON.
    pub fn unreadable(detail: String, class: Option<budget::AssetClass>) -> Report {
        Report {
            diags: vec![Diag {
                code: "R_IO",
                detail,
            }],
            warns: None,
            summary: "unreadable".to_string(),
            class,
        }
    }

    pub fn failed(&self) -> bool {
        !self.diags.is_empty()
    }

    pub fn warnings(&self) -> &[Diag] {
        self.warns.as_deref().unwrap_or(&[])
    }

    pub fn to_json(&self, path: &std::path::Path) -> String {
        let diags: Vec<Value> = self
            .diags
            .iter()
            .map(|d| json!({"code": d.code, "detail": d.detail}))
            .collect();
        let mut map = serde_json::Map::new();
        map.insert("file".to_string(), json!(path.display().to_string()));
        map.insert("ok".to_string(), json!(!self.failed()));
        map.insert("summary".to_string(), json!(self.summary));
        map.insert("diags".to_string(), Value::Array(diags));
        // No "class" key unless --class was given: unclassed runs keep
        // their exact JSON shape.
        if let Some(c) = self.class {
            map.insert("class".to_string(), json!(c.name()));
        }
        // No "warns" key at all unless budgets ran: plain runs keep the
        // v0.3.0 JSON shape byte-for-byte.
        if let Some(warns) = &self.warns {
            let warns: Vec<Value> = warns
                .iter()
                .map(|d| json!({"code": d.code, "detail": d.detail}))
                .collect();
            map.insert("warns".to_string(), Value::Array(warns));
        }
        Value::Object(map).to_string()
    }
}

fn node_name(nodes: &[Value], idx: usize) -> Option<&str> {
    nodes
        .get(idx)
        .and_then(|n| n.get("name"))
        .and_then(Value::as_str)
}

#[cfg(test)]
pub fn check_glb(bytes: &[u8]) -> Report {
    check_glb_with_budget(bytes, None)
}

#[cfg(test)]
pub fn check_glb_with_budget(bytes: &[u8], budget: Option<&budget::Budget>) -> Report {
    check_glb_with_class(bytes, budget, None)
}

#[cfg(test)]
pub fn check_glb_with_class(
    bytes: &[u8],
    budget: Option<&budget::Budget>,
    class: Option<budget::AssetClass>,
) -> Report {
    check_glb_full(bytes, budget, class, &bevy::BevyOpts::default())
}

pub fn check_glb_full(
    bytes: &[u8],
    budget: Option<&budget::Budget>,
    class: Option<budget::AssetClass>,
    opts: &bevy::BevyOpts,
) -> Report {
    let doc = match glb::parse(bytes) {
        Ok(d) => d,
        Err((code, detail)) => {
            return Report {
                diags: vec![Diag { code, detail }],
                warns: None,
                summary: "invalid container".to_string(),
                class,
            };
        }
    };
    let json = &doc.json;
    let nodes = arr(json, "nodes");
    let skins = arr(json, "skins");
    let n_meshes = arr(json, "meshes").len();
    let clips = arr(json, "animations");

    let mut diags: Vec<Diag> = Vec::new();

    // Props and weapons are legitimately unrigged: without a skeleton
    // there is nothing to require and no DEF rule to check anims
    // against. Rigged props still face the full contract below.
    let relaxed = skins.is_empty() && class.is_some_and(|c| !c.needs_skeleton());

    if skins.is_empty() && !relaxed {
        diags.push(Diag {
            code: "R_NO_SKIN",
            detail: "no skins: rig GLB must contain a skeleton".to_string(),
        });
    }

    let mut joints: Vec<String> = Vec::new();
    for (si, skin) in skins.iter().enumerate() {
        for j in arr(skin, "joints") {
            let idx = j.as_u64().and_then(|v| usize::try_from(v).ok());
            match idx.and_then(|i| node_name(nodes, i)) {
                Some(name) => joints.push(name.to_string()),
                None => diags.push(Diag {
                    code: "R_JOINT_INDEX",
                    detail: format!("skin {si} references missing node {j}"),
                }),
            }
        }
    }
    if !skins.is_empty() && joints.is_empty() {
        diags.push(Diag {
            code: "R_NO_JOINTS",
            detail: "skin has no joints".to_string(),
        });
    }
    let mut non_def: Vec<String> = joints
        .iter()
        .filter(|n| !glbkit::rig::is_deform_bone(n))
        .cloned()
        .collect();
    non_def.sort();
    non_def.dedup();
    if !non_def.is_empty() {
        diags.push(Diag {
            code: "R_JOINT_PREFIX",
            detail: format!(
                "{} non-DEF joint{}: {}",
                non_def.len(),
                if non_def.len() == 1 { "" } else { "s" },
                join_names(&non_def),
            ),
        });
    }

    if !relaxed {
        for (ci, clip) in clips.iter().enumerate() {
            let mut bad: Vec<String> = Vec::new();
            for ch in arr(clip, "channels") {
                let target = ch.get("target").and_then(|t| t.get("node"));
                let name = target
                    .and_then(|v| v.as_u64())
                    .and_then(|v| usize::try_from(v).ok())
                    .and_then(|i| node_name(nodes, i))
                    .map(str::to_string);
                match name {
                    Some(n) if glbkit::rig::is_deform_bone(&n) => {}
                    Some(n) => bad.push(n),
                    None => bad.push("<missing node>".to_string()),
                }
            }
            bad.sort();
            bad.dedup();
            if !bad.is_empty() {
                diags.push(Diag {
                    code: "R_ANIM_TARGET",
                    detail: format!("clip {ci} targets non-DEF nodes: {}", join_names(&bad)),
                });
            }
        }
    }

    let (wdiags, skin) = weights::check_weights(json, doc.bin);
    for (code, detail) in wdiags {
        diags.push(Diag { code, detail });
    }

    for (code, detail) in defects::check_defects(json, doc.bin) {
        diags.push(Diag { code, detail });
    }

    for (code, detail) in bevy::check_bevy(json, doc.bin, opts) {
        diags.push(Diag { code, detail });
    }

    let warns: Option<Vec<Diag>> = budget.map(|b| {
        budget::check_budgets(json, doc.bin, b, class, &skin)
            .into_iter()
            .map(|(code, detail)| Diag { code, detail })
            .collect()
    });

    let mut summary = format!(
        "{} joint{}, {} mesh{}, {} clip{}",
        joints.len(),
        if joints.len() == 1 { "" } else { "s" },
        n_meshes,
        if n_meshes == 1 { "" } else { "es" },
        clips.len(),
        if clips.len() == 1 { "" } else { "s" },
    );
    if let Some(c) = class {
        summary.push_str(&format!(", class {}", c.name()));
    }
    if skin.checked_prims > 0 {
        summary.push_str(&format!(", max {} infl/vert", skin.max_influences));
    }
    if let Some(w) = &warns {
        if !w.is_empty() {
            // Not "over budget": P_TEX_NORMAL / P_WEAPON_* / P_PIECE_BONES
            // are convention warnings with no budget behind them.
            let n = w.len();
            summary.push_str(&format!(", {n} {}", plural(n, "warning", "warnings")));
        }
    }
    Report {
        diags,
        warns,
        summary,
        class,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn pack(doc: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(doc).unwrap();
        let total = 12 + 8 + body.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x4E4F534Au32.to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn codes(report: &Report) -> Vec<&'static str> {
        report.diags.iter().map(|d| d.code).collect()
    }

    #[test]
    fn container_rejects_garbage() {
        assert_eq!(codes(&check_glb(b"nope")), vec!["D_GLB_MAGIC"]);
        assert_eq!(codes(&check_glb(b"glTF")), vec!["D_GLB_MAGIC"]);
    }

    #[test]
    fn container_rejects_bad_version() {
        let mut b = pack(&json!({}));
        b[4] = 1;
        assert_eq!(codes(&check_glb(&b)), vec!["D_GLB_VERSION"]);
    }

    #[test]
    fn container_rejects_truncation() {
        let mut b = pack(&json!({}));
        let len = b.len() as u32 + 100;
        b[8..12].copy_from_slice(&len.to_le_bytes());
        assert_eq!(codes(&check_glb(&b)), vec!["D_GLB_TRUNC"]);
    }

    #[test]
    fn container_chunks_must_fit_declared_length() {
        let doc = json!({"nodes": [{"name": "DEF-a"}], "skins": [{"joints": [0]}]});
        // Declared length cuts into the JSON chunk.
        let mut b = pack(&doc);
        let short = (b.len() - 4) as u32;
        b[8..12].copy_from_slice(&short.to_le_bytes());
        assert_eq!(codes(&check_glb(&b)), vec!["D_GLB_TRUNC"]);
        // Declared length drops the BIN chunk: a container defect, not
        // a misleading W_BAD_ACCESSOR on an "empty" BIN.
        let mut b = pack_bin(&doc, &[0u8; 8]);
        let json_only = (b.len() - 16) as u32;
        b[8..12].copy_from_slice(&json_only.to_le_bytes());
        assert!(codes(&check_glb(&b)).is_empty(), "BIN-less prefix is valid");
        let mut b = pack_bin(&doc, &[0u8; 8]);
        let mid_bin = (b.len() - 4) as u32;
        b[8..12].copy_from_slice(&mid_bin.to_le_bytes());
        assert_eq!(codes(&check_glb(&b)), vec!["D_GLB_TRUNC"]);
        // Trailing bytes past the declared length are ignored.
        let mut b = pack(&doc);
        b.extend_from_slice(b"junk");
        assert!(codes(&check_glb(&b)).is_empty());
    }

    #[test]
    fn unreadable_json_escapes_path() {
        use budget::AssetClass::Hero;
        let r = Report::unreadable("cannot read file: \"gone\"".to_string(), Some(Hero));
        assert!(r.failed());
        let p = Path::new("C:\\assets\\\"hero\".glb");
        let v: Value = serde_json::from_str(&r.to_json(p)).expect("valid JSON");
        assert_eq!(v["file"], "C:\\assets\\\"hero\".glb");
        assert_eq!(v["ok"], false);
        assert_eq!(v["class"], "hero");
        assert_eq!(v["diags"][0]["code"], "R_IO");
        assert_eq!(v["diags"][0]["detail"], "cannot read file: \"gone\"");
        assert!(v.get("warns").is_none());
    }

    #[test]
    fn clean_rig_passes() {
        let doc = json!({
            "nodes": [{"name": "DEF-spine"}, {"name": "Cube", "mesh": 0}],
            "scenes": [{"nodes": [0, 1]}],
            "meshes": [{"name": "Cube", "primitives": [{}]}],
            "skins": [{"joints": [0]}],
            "animations": [{
                "channels": [{"target": {"node": 0}}],
                "samplers": [{}],
            }],
        });
        let r = check_glb(&pack(&doc));
        assert!(
            r.diags.is_empty(),
            "unexpected diags: {:?}",
            r.diags.iter().map(|d| &d.detail).collect::<Vec<_>>()
        );
        assert_eq!(r.summary, "1 joint, 1 mesh, 1 clip");
    }

    #[test]
    fn flags_control_leaks_and_anim_targets() {
        let doc = json!({
            "nodes": [
                {"name": "DEF-spine"},
                {"name": "torso"},
                {"name": "Cube", "mesh": 0},
            ],
            "meshes": [{"name": "Cube"}],
            "skins": [{"joints": [0, 1]}],
            "animations": [{"channels": [
                {"target": {"node": 0}},
                {"target": {"node": 2}},
            ]}],
        });
        let found = codes(&check_glb(&pack(&doc)));
        assert!(
            found.contains(&"R_JOINT_PREFIX"),
            "missing PREFIX: {found:?}"
        );
        assert!(
            found.contains(&"R_ANIM_TARGET"),
            "missing TARGET: {found:?}"
        );
    }

    #[test]
    fn flags_missing_skin_and_joints() {
        let r = check_glb(&pack(&json!({"nodes": [], "meshes": []})));
        assert!(codes(&r).contains(&"R_NO_SKIN"));
        let r2 = check_glb(&pack(&json!({"nodes": [], "skins": [{"joints": []}]})));
        assert!(codes(&r2).contains(&"R_NO_JOINTS"));
    }

    #[test]
    fn flags_dangling_joint_index() {
        let r = check_glb(&pack(&json!({"nodes": [], "skins": [{"joints": [7]}]})));
        assert!(codes(&r).contains(&"R_JOINT_INDEX"));
    }

    fn pack_bin(doc: &Value, bin: &[u8]) -> Vec<u8> {
        let mut body = serde_json::to_vec(doc).unwrap();
        while !body.len().is_multiple_of(4) {
            body.push(b' ');
        }
        let mut blob = bin.to_vec();
        while !blob.len().is_multiple_of(4) {
            blob.push(0);
        }
        let total = 12 + 8 + body.len() + 8 + blob.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x4E4F534Au32.to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x004E4942u32.to_le_bytes());
        out.extend_from_slice(&blob);
        out
    }

    fn skel_doc(attrs: Value, accessors: Value, views: Value, buflen: usize) -> Value {
        json!({
            "nodes": [
                {"name": "DEF-a"}, {"name": "DEF-b"},
                {"name": "DEF-c"}, {"name": "DEF-d"},
                {"name": "Body", "mesh": 0, "skin": 0},
            ],
            "meshes": [{"name": "Body", "primitives": [{"attributes": attrs}]}],
            "skins": [{"joints": [0, 1, 2, 3]}],
            "accessors": accessors,
            "bufferViews": views,
            "buffers": [{"byteLength": buflen}],
        })
    }

    fn f32s(xs: &[f32]) -> Vec<u8> {
        let mut b = Vec::with_capacity(xs.len() * 4);
        for x in xs {
            b.extend_from_slice(&x.to_le_bytes());
        }
        b
    }

    fn detail_for<'a>(r: &'a Report, code: &str) -> Option<&'a str> {
        r.diags
            .iter()
            .find(|d| d.code == code)
            .map(|d| d.detail.as_str())
    }

    fn diags_str(r: &Report) -> Vec<&str> {
        r.diags.iter().map(|d| d.detail.as_str()).collect()
    }

    #[test]
    fn weights_clean_reports_max() {
        let mut bin = vec![
            0, 1, 2, 3, // v0 joints
            0, 1, 0, 0, // v1
            2, 0, 0, 0, // v2
        ];
        bin.extend_from_slice(&f32s(&[
            0.25, 0.25, 0.25, 0.25, // v0: 4 influences, sum 1
            0.5, 0.5, 0.0, 0.0, // v1: 2 influences
            1.0, 0.0, 0.0, 0.0, // v2: 1 influence
        ]));
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 3},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 3},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 12},
                {"byteOffset": 12, "byteLength": 48},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        assert!(r.diags.is_empty(), "unexpected: {:?}", diags_str(&r));
        assert_eq!(r.summary, "4 joints, 1 mesh, 0 clips, max 4 infl/vert");
    }

    #[test]
    fn weights_over_limit_second_set() {
        let mut bin = vec![
            0, 0, 0, 0, // v0 joints0
            0, 1, 2, 3, // v1
            0, 1, 0, 0, // v2
        ];
        bin.extend_from_slice(&f32s(&[
            1.0, 0.0, 0.0, 0.0, //
            0.2, 0.2, 0.2, 0.2, // v1 set0: 4 inf, sum 0.8
            0.5, 0.5, 0.0, 0.0, //
        ]));
        bin.extend_from_slice(&[0u8; 12]); // joints1
        bin.extend_from_slice(&f32s(&[
            0.0, 0.0, 0.0, 0.0, //
            0.2, 0.0, 0.0, 0.0, // v1 set1: 5th influence, sum back to 1
            0.0, 0.0, 0.0, 0.0, //
        ]));
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1, "JOINTS_1": 2, "WEIGHTS_1": 3}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 3},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 3},
                {"bufferView": 2, "componentType": 5121, "type": "VEC4", "count": 3},
                {"bufferView": 3, "componentType": 5126, "type": "VEC4", "count": 3},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 12},
                {"byteOffset": 12, "byteLength": 48},
                {"byteOffset": 60, "byteLength": 12},
                {"byteOffset": 72, "byteLength": 48},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        let d = detail_for(&r, "W_OVER_INFLUENCE").expect("missing W_OVER_INFLUENCE");
        assert!(d.contains("vertex 1"), "no vertex detail: {d}");
        assert!(d.contains("with 5"), "no count detail: {d}");
        assert!(
            detail_for(&r, "W_UNNORMALIZED").is_none(),
            "sums are 1.0, must not flag: {:?}",
            diags_str(&r)
        );
        assert!(
            r.summary.ends_with("max 5 infl/vert"),
            "summary: {}",
            r.summary
        );
    }

    #[test]
    fn weights_unnormalized() {
        let mut bin = vec![0, 1, 2, 3, 0, 1, 0, 0, 2, 0, 0, 0];
        bin.extend_from_slice(&f32s(&[
            0.25, 0.25, 0.25, 0.25, //
            0.5, 0.5, 0.0, 0.0, //
            0.25, 0.25, 0.0, 0.0, // v2 sums to 0.5
        ]));
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 3},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 3},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 12},
                {"byteOffset": 12, "byteLength": 48},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        let d = detail_for(&r, "W_UNNORMALIZED").expect("missing W_UNNORMALIZED");
        assert!(d.contains("vertex 2"), "no vertex detail: {d}");
        assert!(d.contains("sum=0.500"), "no sum detail: {d}");
        assert!(detail_for(&r, "W_OVER_INFLUENCE").is_none());
    }

    #[test]
    fn weights_normalized_ubyte_stride_and_ushort_joints() {
        // USHORT joints, normalized-UBYTE weights with byteStride 8
        // (4 data + 4 pad) and split view/accessor offsets (14 + 2).
        let mut bin: Vec<u8> = Vec::new();
        for j in [0u16, 1, 2, 3, 3, 0, 0, 0] {
            bin.extend_from_slice(&j.to_le_bytes());
        }
        bin.extend_from_slice(&[255, 0, 0, 0, 0, 0, 0, 0]); // v0: sum 1
        bin.extend_from_slice(&[128, 64, 32, 31, 0, 0, 0, 0]); // v1: 255/255
        assert_eq!(bin.len(), 32);
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5123, "type": "VEC4", "count": 2},
                {"bufferView": 1, "byteOffset": 2, "componentType": 5121,
                 "normalized": true, "type": "VEC4", "count": 2},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 16},
                {"byteOffset": 14, "byteStride": 8, "byteLength": 14},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        assert!(r.diags.is_empty(), "unexpected: {:?}", diags_str(&r));
        assert!(
            r.summary.ends_with("max 4 infl/vert"),
            "summary: {}",
            r.summary
        );
    }

    #[test]
    fn weights_normalized_ushort() {
        let bin_joints = [0u8, 1, 2, 3];
        let mut bin = bin_joints.to_vec();
        for w in [32768u16, 16384, 8192, 8191] {
            // sums to 65535 -> 1.0
            bin.extend_from_slice(&w.to_le_bytes());
        }
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 1},
                {"bufferView": 1, "componentType": 5123,
                 "normalized": true, "type": "VEC4", "count": 1},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 4},
                {"byteOffset": 4, "byteLength": 8},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        assert!(r.diags.is_empty(), "unexpected: {:?}", diags_str(&r));
    }

    #[test]
    fn weights_sparse_is_explicit() {
        let mut bin = vec![0u8; 12];
        bin.extend_from_slice(&f32s(&[0.0; 12]));
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 3},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 3,
                 "sparse": {"count": 1}},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 12},
                {"byteOffset": 12, "byteLength": 48},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        assert!(
            detail_for(&r, "W_SPARSE").is_some_and(|d| d.contains("sparse")),
            "sparse must be explicit: {:?}",
            diags_str(&r)
        );
        // Skipped, never misread: no weight verdicts on sparse data.
        assert!(detail_for(&r, "W_UNNORMALIZED").is_none());
        assert!(detail_for(&r, "W_OVER_INFLUENCE").is_none());
    }

    #[test]
    fn weights_rejects_non_normalized_int() {
        // UNSIGNED_BYTE weights without normalized=true would read as
        // 0..255 floats; that must be an error, never a silent misread.
        let mut bin = vec![0u8; 12];
        bin.extend_from_slice(&[64, 64, 64, 63, 0, 0, 0, 0, 0, 0, 0, 0]);
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 3},
                {"bufferView": 1, "componentType": 5121, "type": "VEC4", "count": 3},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 12},
                {"byteOffset": 12, "byteLength": 12},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        assert!(codes(&r).contains(&"W_BAD_ACCESSOR"), "{:?}", diags_str(&r));
        assert!(detail_for(&r, "W_UNNORMALIZED").is_none());
    }

    #[test]
    fn weights_missing_attrs_and_bad_shapes() {
        // Skinned primitive with no skinning attributes at all.
        let doc = skel_doc(
            json!({"POSITION": 0}),
            json!([
                {"bufferView": 0, "componentType": 5126, "type": "VEC3", "count": 3},
            ]),
            json!([{"byteOffset": 0, "byteLength": 36}]),
            36,
        );
        let r = check_glb(&pack_bin(&doc, &[0u8; 36]));
        assert!(
            detail_for(&r, "W_BAD_ACCESSOR").is_some_and(|d| d.contains("lacks JOINTS_0")),
            "{:?}",
            diags_str(&r)
        );

        // JOINTS/WEIGHTS counts disagree.
        let mut bin = vec![0u8; 12];
        bin.extend_from_slice(&f32s(&[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]));
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 3},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 2},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 12},
                {"byteOffset": 12, "byteLength": 32},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        assert!(
            detail_for(&r, "W_BAD_ACCESSOR").is_some_and(|d| d.contains("counts disagree")),
            "{:?}",
            diags_str(&r)
        );

        // Unpaired second set.
        let mut bin = vec![0u8; 12];
        bin.extend_from_slice(&f32s(&[1.0, 0.0, 0.0, 0.0]));
        bin.extend_from_slice(&[0u8; 4]);
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1, "JOINTS_1": 2}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 1},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 1},
                {"bufferView": 2, "componentType": 5121, "type": "VEC4", "count": 1},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 12},
                {"byteOffset": 12, "byteLength": 16},
                {"byteOffset": 28, "byteLength": 4},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        assert!(
            detail_for(&r, "W_BAD_ACCESSOR").is_some_and(|d| d.contains("JOINTS_1")),
            "{:?}",
            diags_str(&r)
        );
    }

    fn anim_doc(nan: bool) -> (Value, Vec<u8>) {
        let doc = json!({
            "nodes": [{"name": "DEF-a"}],
            "scenes": [{"nodes": [0]}],
            "skins": [{"joints": [0]}],
            "animations": [{
                "channels": [{"target": {"node": 0}}],
                "samplers": [{"input": 0, "output": 1}],
            }],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "type": "SCALAR", "count": 2},
                {"bufferView": 1, "componentType": 5126, "type": "VEC3", "count": 2},
            ],
            "bufferViews": [
                {"byteOffset": 0, "byteLength": 8},
                {"byteOffset": 8, "byteLength": 24},
            ],
            "buffers": [{"byteLength": 32}],
        });
        let mut bin = f32s(&[0.0, 1.0]);
        let tail = if nan {
            [0.0, 0.0, 0.0, f32::NAN, f32::INFINITY, 1.0]
        } else {
            [0.0, 0.0, 0.0, 1.0, 2.0, 3.0]
        };
        bin.extend_from_slice(&f32s(&tail));
        (doc, bin)
    }

    #[test]
    fn defects_anim_nan() {
        let (doc, bin) = anim_doc(true);
        let r = check_glb(&pack_bin(&doc, &bin));
        let d = detail_for(&r, "X_ANIM_NAN").expect("missing X_ANIM_NAN");
        assert!(d.contains("clip 0 sampler 0"), "no sampler tag: {d}");
        assert!(d.contains("accessor 1"), "no accessor ref: {d}");
        assert!(d.contains("2 non-finite"), "no count: {d}");
        assert!(d.contains("component 3"), "no first index: {d}");
        assert!(detail_for(&r, "X_ANIM_KEYS").is_none());
    }

    #[test]
    fn defects_anim_clean_passes() {
        let (doc, bin) = anim_doc(false);
        let r = check_glb(&pack_bin(&doc, &bin));
        assert!(r.diags.is_empty(), "unexpected: {:?}", diags_str(&r));
        assert_eq!(r.summary, "1 joint, 0 meshes, 1 clip");
    }

    #[test]
    fn defects_anim_keys() {
        let doc = json!({
            "nodes": [{"name": "DEF-a"}],
            "skins": [{"joints": [0]}],
            "animations": [{
                "channels": [{"target": {"node": 0}}],
                "samplers": [{"input": 0, "output": 1}],
            }],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "type": "SCALAR", "count": 0},
                {"bufferView": 1, "componentType": 5126, "type": "VEC3", "count": 1},
            ],
            "bufferViews": [
                {"byteOffset": 0, "byteLength": 0},
                {"byteOffset": 0, "byteLength": 12},
            ],
            "buffers": [{"byteLength": 12}],
        });
        let r = check_glb(&pack_bin(&doc, &f32s(&[1.0, 2.0, 3.0])));
        let d = detail_for(&r, "X_ANIM_KEYS").expect("missing X_ANIM_KEYS");
        assert!(d.contains("0 keyframes"), "no count: {d}");
        assert!(detail_for(&r, "X_ANIM_NAN").is_none());
        assert!(detail_for(&r, "X_ACCESSOR_BOUNDS").is_none());
    }

    #[test]
    fn defects_joint_range() {
        let mut bin = vec![0, 1, 2, 9]; // index 9: skin has 4 joints
        bin.extend_from_slice(&f32s(&[0.25, 0.25, 0.25, 0.25]));
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 1},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 1},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 4},
                {"byteOffset": 4, "byteLength": 16},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        let d = detail_for(&r, "X_JOINT_RANGE").expect("missing X_JOINT_RANGE");
        assert!(d.contains("JOINTS_0"), "no set tag: {d}");
        assert!(d.contains("vertex 0 index 9"), "no location: {d}");
        assert!(d.contains("has 4 joints"), "no skin size: {d}");
        assert!(
            !codes(&r).iter().any(|c| c.starts_with("W_")),
            "weights are valid: {:?}",
            diags_str(&r)
        );
    }

    #[test]
    fn defects_accessor_bounds() {
        let doc = json!({
            "nodes": [{"name": "DEF-a"}],
            "skins": [{"joints": [0]}],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "type": "VEC3", "count": 3},
                {"bufferView": 1, "componentType": 5126, "type": "VEC3", "count": 10},
                {"bufferView": 2, "componentType": 5126, "type": "SCALAR", "count": 1},
                {"bufferView": 3, "componentType": 5126, "type": "VEC3", "count": 1},
            ],
            "bufferViews": [
                {"byteOffset": 0, "byteLength": 36},
                {"byteOffset": 36, "byteLength": 12},
                {"byteOffset": 1000, "byteLength": 100},
                {"byteOffset": 48, "byteLength": 12, "byteStride": 4},
            ],
            "buffers": [{"byteLength": 60}],
        });
        let r = check_glb(&pack_bin(&doc, &[0u8; 60]));
        let found: Vec<&str> = r
            .diags
            .iter()
            .filter(|d| d.code == "X_ACCESSOR_BOUNDS")
            .map(|d| d.detail.as_str())
            .collect();
        assert_eq!(found.len(), 3, "want 3 bounds diags: {found:?}");
        assert!(
            found.iter().any(|d| d.contains("bufferView 1")),
            "no view diag: {found:?}"
        );
        assert!(
            found.iter().any(|d| d.contains("buffer 0 byteLength 60")),
            "no buffer diag: {found:?}"
        );
        assert!(
            found.iter().any(|d| d.contains("byteStride 4")),
            "no stride diag: {found:?}"
        );
    }

    #[test]
    fn defects_empty_mesh_prim() {
        let doc = json!({
            "nodes": [{"name": "DEF-a"}],
            "skins": [{"joints": [0]}],
            "meshes": [
                {"name": "Ghost"},
                {"name": "Flat", "primitives": [
                    {"attributes": {"POSITION": 0}, "indices": 1},
                ]},
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "type": "VEC3", "count": 0},
                {"bufferView": 1, "componentType": 5123, "type": "SCALAR", "count": 0},
            ],
            "bufferViews": [
                {"byteOffset": 0, "byteLength": 0},
                {"byteOffset": 0, "byteLength": 0},
            ],
            "buffers": [{"byteLength": 0}],
        });
        let r = check_glb(&pack_bin(&doc, &[]));
        let meshes = codes(&r).iter().filter(|c| **c == "X_EMPTY_MESH").count();
        assert_eq!(meshes, 2, "want both mesh diags: {:?}", diags_str(&r));
        let d = detail_for(&r, "X_EMPTY_PRIM").expect("missing X_EMPTY_PRIM");
        assert!(d.contains("zero indices"), "wrong prim detail: {d}");
        assert!(detail_for(&r, "X_ACCESSOR_BOUNDS").is_none());
    }

    #[test]
    fn defects_morph_count() {
        let doc = json!({
            "nodes": [{"name": "DEF-a"}],
            "skins": [{"joints": [0]}],
            "meshes": [{
                "name": "Face",
                "weights": [0.0, 0.0],
                "primitives": [{
                    "attributes": {"POSITION": 0},
                    "targets": [{"POSITION": 1}],
                }],
            }],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "type": "VEC3", "count": 5},
                {"bufferView": 1, "componentType": 5126, "type": "VEC3", "count": 3},
            ],
            "bufferViews": [
                {"byteOffset": 0, "byteLength": 60},
                {"byteOffset": 60, "byteLength": 36},
            ],
            "buffers": [{"byteLength": 96}],
        });
        let r = check_glb(&pack_bin(&doc, &[0u8; 96]));
        let found: Vec<&str> = r
            .diags
            .iter()
            .filter(|d| d.code == "X_MORPH_COUNT")
            .map(|d| d.detail.as_str())
            .collect();
        assert_eq!(found.len(), 2, "want both morph diags: {found:?}");
        assert!(
            found.iter().any(|d| d.contains("mesh.weights has 2")),
            "no weights diag: {found:?}"
        );
        assert!(
            found.iter().any(|d| d.contains("POSITION count 5")),
            "no target-count diag: {found:?}"
        );
    }

    fn wcodes(report: &Report) -> Vec<&'static str> {
        report.warnings().iter().map(|d| d.code).collect()
    }

    fn warn_for<'a>(r: &'a Report, code: &str) -> Option<&'a str> {
        r.warnings()
            .iter()
            .find(|d| d.code == code)
            .map(|d| d.detail.as_str())
    }

    fn file_budget(text: &str) -> budget::Budget {
        budget::parse_budget_set(text).unwrap().generic
    }

    fn mesh_budget_doc(verts: u64, idx: u64) -> (Value, Vec<u8>) {
        // POSITION bytes + U16 index bytes, sized so X_ACCESSOR_BOUNDS
        // stays quiet: budget tests must isolate the P_* layer.
        let pv = verts as usize * 12;
        let ib = idx as usize * 2;
        // The hand socket keeps classed hero runs on the P_* under test.
        let doc = json!({
            "nodes": [{"name": "DEF-a", "children": [1]}, {"name": "Socket_Hand_R"}],
            "skins": [{"joints": [0]}],
            "meshes": [{"name": "Hero", "primitives": [
                {"attributes": {"POSITION": 0}, "indices": 1},
            ]}],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "type": "VEC3", "count": verts},
                {"bufferView": 1, "componentType": 5123, "type": "SCALAR", "count": idx},
            ],
            "bufferViews": [
                {"byteOffset": 0, "byteLength": pv},
                {"byteOffset": pv, "byteLength": ib},
            ],
            "buffers": [{"byteLength": pv + ib}],
        });
        (doc, vec![0u8; pv + ib])
    }

    fn png_hdr(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
        b.extend_from_slice(&13u32.to_be_bytes());
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 2, 0, 0, 0]);
        b
    }

    #[test]
    fn mobile_over_budget_mesh_warns_not_fails() {
        let (doc, bin) = mesh_budget_doc(70_000, 300_003);
        let r = check_glb_with_budget(&pack_bin(&doc, &bin), Some(&budget::Budget::default()));
        assert!(r.diags.is_empty(), "must not fail: {:?}", diags_str(&r));
        assert!(!r.failed());
        assert_eq!(wcodes(&r), vec!["P_VERTS", "P_TRIS"]);
        let v = warn_for(&r, "P_VERTS").unwrap();
        assert!(v.contains("70000 verts"), "no count: {v}");
        assert!(v.contains("65535"), "no budget: {v}");
        let t = warn_for(&r, "P_TRIS").unwrap();
        assert!(t.contains("100001 tris"), "no count: {t}");
        assert!(
            r.summary.ends_with(", 2 warnings"),
            "summary: {}",
            r.summary
        );
        let j = r.to_json(Path::new("hero.glb"));
        assert!(j.contains("\"ok\":true"), "json: {j}");
        assert!(j.contains("\"warns\":"), "json: {j}");
    }

    #[test]
    fn mobile_under_budget_passes() {
        let (doc, bin) = mesh_budget_doc(1_000, 3_000);
        let r = check_glb_with_budget(&pack_bin(&doc, &bin), Some(&budget::Budget::default()));
        assert!(r.diags.is_empty(), "unexpected: {:?}", diags_str(&r));
        assert!(r.warnings().is_empty());
        assert!(!r.summary.contains("warning"), "summary: {}", r.summary);
        // Budgets ran (empty warns array proves it), file is clean.
        let j: Value = serde_json::from_str(&r.to_json(Path::new("ok.glb"))).unwrap();
        assert_eq!(j["warns"], json!([]));
        assert_eq!(j["ok"], json!(true));
    }

    #[test]
    fn mobile_points_excluded_from_tris() {
        let (mut doc, _) = mesh_budget_doc(70_000, 0);
        doc["meshes"][0]["primitives"][0]["mode"] = json!(0);
        doc["meshes"][0]["primitives"][0]
            .as_object_mut()
            .unwrap()
            .remove("indices");
        let bin = vec![0u8; 70_000 * 12];
        doc["accessors"] = json!([
            {"bufferView": 0, "componentType": 5126, "type": "VEC3", "count": 70_000},
        ]);
        doc["bufferViews"] = json!([{"byteOffset": 0, "byteLength": 70_000 * 12}]);
        doc["buffers"] = json!([{"byteLength": 70_000 * 12}]);
        let r = check_glb_with_budget(&pack_bin(&doc, &bin), Some(&budget::Budget::default()));
        assert_eq!(wcodes(&r), vec!["P_VERTS"]);
    }

    #[test]
    fn mobile_no_flag_run_unchanged() {
        let (doc, bin) = mesh_budget_doc(70_000, 300_000);
        let bytes = pack_bin(&doc, &bin);
        let r = check_glb(&bytes);
        assert!(r.warns.is_none(), "no budget output unless asked");
        assert!(r.diags.is_empty(), "unexpected: {:?}", diags_str(&r));
        assert_eq!(r.summary, "1 joint, 1 mesh, 0 clips");
        // v0.3.0 JSON shape byte-for-byte: no "warns" key at all.
        let j: Value = serde_json::from_str(&r.to_json(Path::new("hero.glb"))).unwrap();
        let keys: Vec<&str> = j.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["diags", "file", "ok", "summary"]);
    }

    #[test]
    fn mobile_custom_budget_overrides() {
        let (sdoc, sbin) = mesh_budget_doc(1_000, 3_000);
        let tight = file_budget(r#"{"max_verts_per_mesh": 500}"#);
        let r = check_glb_with_budget(&pack_bin(&sdoc, &sbin), Some(&tight));
        assert!(r.diags.is_empty());
        assert_eq!(wcodes(&r), vec!["P_VERTS"]); // tris still under default
        let (bdoc, bbin) = mesh_budget_doc(70_000, 300_000);
        let loose = file_budget("max_verts_per_mesh = 999999\nmax_tris_per_mesh = 999999\n");
        let r = check_glb_with_budget(&pack_bin(&bdoc, &bbin), Some(&loose));
        assert!(r.diags.is_empty());
        assert!(r.warnings().is_empty(), "loose budget must pass");
    }

    #[test]
    fn mobile_influences_warn_not_fail() {
        // 1 vertex, 4 influences: passes the rig contract exactly.
        let mut bin = vec![0, 1, 2, 3];
        bin.extend_from_slice(&f32s(&[0.25, 0.25, 0.25, 0.25]));
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 1},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 1},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 4},
                {"byteOffset": 4, "byteLength": 16},
            ]),
            bin.len(),
        );
        let bytes = pack_bin(&doc, &bin);
        let base = check_glb(&bytes);
        assert!(base.diags.is_empty(), "unexpected: {:?}", diags_str(&base));
        assert_eq!(base.summary, "4 joints, 1 mesh, 0 clips, max 4 infl/vert");
        // Default mobile budget agrees (4 is the phone standard); a
        // low-end override warns while the contract still passes.
        let strict = file_budget(r#"{"max_influences": 2}"#);
        let r = check_glb_with_budget(&bytes, Some(&strict));
        assert!(
            r.diags.is_empty(),
            "budget must not fail: {:?}",
            diags_str(&r)
        );
        assert!(!r.failed());
        assert_eq!(wcodes(&r), vec!["P_INFLUENCES"]);
        let d = warn_for(&r, "P_INFLUENCES").unwrap();
        assert!(d.contains("max 4 infl/vert"), "no count: {d}");
        assert!(d.contains("exceeds 2 budget"), "no budget: {d}");
        let r = check_glb_with_budget(&bytes, Some(&budget::Budget::default()));
        assert!(r.warnings().is_empty(), "default allows 4 influences");
    }

    #[test]
    fn mobile_texture_warn_and_skip() {
        let img_doc = |images: Value, blen: usize| {
            json!({
                "nodes": [{"name": "DEF-a"}],
                "skins": [{"joints": [0]}],
                "images": images,
                "bufferViews": [{"byteOffset": 0, "byteLength": blen}],
                "buffers": [{"byteLength": blen}],
            })
        };
        let big = png_hdr(4096, 4096);
        let doc = img_doc(
            json!([{"name": "Big", "bufferView": 0, "mimeType": "image/png"}]),
            big.len(),
        );
        let r = check_glb_with_budget(&pack_bin(&doc, &big), Some(&budget::Budget::default()));
        assert!(r.diags.is_empty(), "unexpected: {:?}", diags_str(&r));
        assert_eq!(wcodes(&r), vec!["P_TEX_SIZE"]);
        let d = warn_for(&r, "P_TEX_SIZE").unwrap();
        assert!(d.contains("4096x4096"), "no dims: {d}");
        assert!(d.contains("2048px"), "no budget: {d}");
        // Small embedded PNG + external URI: measured-pass and
        // unmeasured-skip are both silent.
        let small = png_hdr(64, 64);
        let doc = img_doc(
            json!([
                {"name": "Small", "bufferView": 0, "mimeType": "image/png"},
                {"name": "Ext", "uri": "https://example.com/tex.png"},
            ]),
            small.len(),
        );
        let r = check_glb_with_budget(&pack_bin(&doc, &small), Some(&budget::Budget::default()));
        assert!(r.diags.is_empty(), "unexpected: {:?}", diags_str(&r));
        assert!(r.warnings().is_empty());
    }

    fn classed(bytes: &[u8], class: budget::AssetClass) -> Report {
        let b = budget::Budget::mobile_for(class);
        check_glb_with_class(bytes, Some(&b), Some(class))
    }

    fn joints_doc(def: usize, other: usize) -> Value {
        let mut nodes = Vec::new();
        let mut joints = Vec::new();
        for i in 0..def {
            nodes.push(json!({"name": format!("DEF-b{i}")}));
            joints.push(json!(i));
        }
        for i in 0..other {
            nodes.push(json!({"name": format!("ctrl{i}")}));
            joints.push(json!(def + i));
        }
        if def > 0 {
            // Hand socket under the first bone: heroes need one.
            nodes[0]["children"] = json!([nodes.len()]);
            nodes.push(json!({"name": "Socket_Hand_R"}));
        }
        json!({
            "nodes": nodes,
            "skins": [{"joints": joints}],
            "materials": [{"normalTexture": {"index": 0}}],
        })
    }

    #[test]
    fn class_hero_mesh_budget_names_class() {
        use budget::AssetClass::Hero;
        // Over both hero ceilings (15k tris, 10k verts), under the
        // generic flagship ones: only a classed run warns.
        let (mut doc, bin) = mesh_budget_doc(12_000, 48_000);
        // A normal map, so the test isolates mesh budgets (heroes
        // without one earn P_TEX_NORMAL, covered separately).
        doc["materials"] = json!([{"normalTexture": {"index": 0}}]);
        let bytes = pack_bin(&doc, &bin);
        let r = classed(&bytes, Hero);
        assert!(r.diags.is_empty(), "must not fail: {:?}", diags_str(&r));
        assert_eq!(wcodes(&r), vec!["P_VERTS", "P_TRIS"]);
        let v = warn_for(&r, "P_VERTS").unwrap();
        assert!(v.contains("12000 verts"), "no count: {v}");
        assert!(v.contains("10000 hero budget"), "no class budget: {v}");
        let t = warn_for(&r, "P_TRIS").unwrap();
        assert!(t.contains("16000 tris"), "no count: {t}");
        assert!(t.contains("15000 hero budget"), "no class budget: {t}");
        assert!(r.summary.contains("class hero"), "summary: {}", r.summary);
        let j: Value = serde_json::from_str(&r.to_json(Path::new("hero.glb"))).unwrap();
        assert_eq!(j["class"], json!("hero"));
        assert_eq!(j["ok"], json!(true));
        // Same file, unclassed mobile run: flagship ceilings pass.
        let r = check_glb_with_budget(&bytes, Some(&budget::Budget::default()));
        assert!(r.warnings().is_empty(), "generic must pass");
        assert!(r.to_json(Path::new("hero.glb")).find("\"class\"").is_none());
    }

    #[test]
    fn class_top_level_fallback_applies() {
        use budget::AssetClass::Hero;
        // Old flat file means "every asset", classed runs included.
        let set = budget::parse_budget_set("max_tris_per_mesh = 50000\n").unwrap();
        let (mut doc, bin) = mesh_budget_doc(1_000, 180_000); // 60k tris
        doc["materials"] = json!([{"normalTexture": {"index": 0}}]);
        let r = check_glb_with_class(
            &pack_bin(&doc, &bin),
            Some(set.for_class(Some(Hero))),
            Some(Hero),
        );
        assert!(r.diags.is_empty());
        assert_eq!(wcodes(&r), vec!["P_TRIS"]);
        let t = warn_for(&r, "P_TRIS").unwrap();
        assert!(t.contains("50000 hero budget"), "no fallback: {t}");
    }

    #[test]
    fn class_bones_count_def_only() {
        use budget::AssetClass::Hero;
        // 162 deform bones over the 160 hero ceiling; 5 control
        // joints fail the contract but do not join the bone count.
        let r = classed(&pack(&joints_doc(162, 5)), Hero);
        assert!(codes(&r).contains(&"R_JOINT_PREFIX"));
        let d = warn_for(&r, "P_BONES").expect("missing P_BONES");
        assert!(d.contains("162 deform bones"), "no count: {d}");
        assert!(d.contains("160 hero budget"), "no budget: {d}");
        // The R_* fails; the P_* only warns.
        assert!(r.failed());
        // Small rig under every ceiling stays silent.
        let r = classed(&pack(&joints_doc(4, 0)), Hero);
        assert!(r.diags.is_empty(), "unexpected: {:?}", diags_str(&r));
        assert!(r.warnings().is_empty(), "unexpected warns");
        // Unclassed default (256) tolerates mid-size rigs.
        let r = check_glb_with_budget(&pack(&joints_doc(200, 0)), Some(&budget::Budget::default()));
        assert!(warn_for(&r, "P_BONES").is_none());
        let tight = file_budget("max_bones = 100\n");
        let r = check_glb_with_budget(&pack(&joints_doc(200, 0)), Some(&tight));
        let d = warn_for(&r, "P_BONES").expect("missing P_BONES");
        assert!(
            d.contains("200 deform bones exceed 100 budget"),
            "detail: {d}"
        );
    }

    #[test]
    fn class_normal_map_characters_only() {
        use budget::AssetClass::*;
        let bare = json!({
            "nodes": [{"name": "DEF-a", "children": [1]}, {"name": "Socket_Hand_R"}],
            "skins": [{"joints": [0]}],
        });
        let r = classed(&pack(&bare), Hero);
        let d = warn_for(&r, "P_TEX_NORMAL").expect("hero needs P_TEX_NORMAL");
        assert!(d.contains("normalTexture"), "detail: {d}");
        assert!(r.diags.is_empty(), "warn only: {:?}", diags_str(&r));
        assert!(r.summary.ends_with(", 1 warning"), "summary: {}", r.summary);
        let r = classed(&pack(&bare), Npc);
        assert!(warn_for(&r, "P_TEX_NORMAL").is_some());
        // Props, weapons, and unclassed runs skip (nothing provable).
        let r = classed(&pack(&bare), Prop);
        assert!(warn_for(&r, "P_TEX_NORMAL").is_none());
        let r = classed(&pack(&bare), Weapon);
        assert!(warn_for(&r, "P_TEX_NORMAL").is_none());
        let r = check_glb_with_budget(&pack(&bare), Some(&budget::Budget::default()));
        assert!(warn_for(&r, "P_TEX_NORMAL").is_none());
        // Any material with a normalTexture satisfies the check.
        let mut with_normal = bare.clone();
        with_normal["materials"] = json!([
            {"name": "skin"},
            {"name": "cloth", "normalTexture": {"index": 0}},
        ]);
        let r = classed(&pack(&with_normal), Hero);
        assert!(warn_for(&r, "P_TEX_NORMAL").is_none());
    }

    fn piece_doc() -> Value {
        json!({
            "nodes": [
                {"name": "DEF-a"}, {"name": "DEF-b"},
                {"name": "DEF-c"}, {"name": "DEF-d"},
                {"name": "Body", "mesh": 0, "skin": 0},
                {"name": "Cape", "mesh": 1, "skin": 0},
            ],
            "meshes": [
                {"name": "Body",
                 "primitives": [{"attributes": {"JOINTS_0": 0, "WEIGHTS_0": 1}}]},
                {"name": "Cape",
                 "primitives": [{"attributes": {"JOINTS_0": 2, "WEIGHTS_0": 3}}]},
            ],
            "skins": [{"joints": [0, 1, 2, 3]}],
            "accessors": [
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 1},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 1},
                {"bufferView": 2, "componentType": 5121, "type": "VEC4", "count": 1},
                {"bufferView": 3, "componentType": 5126, "type": "VEC4", "count": 1},
            ],
            "bufferViews": [
                {"byteOffset": 0, "byteLength": 4},
                {"byteOffset": 4, "byteLength": 16},
                {"byteOffset": 20, "byteLength": 4},
                {"byteOffset": 24, "byteLength": 16},
            ],
            "buffers": [{"byteLength": 40}],
        })
    }

    fn piece_bin(cape_joint: u8) -> Vec<u8> {
        let mut bin = vec![0, 1, 0, 0]; // body: joints 0+1
        bin.extend_from_slice(&f32s(&[0.5, 0.5, 0.0, 0.0]));
        bin.extend_from_slice(&[cape_joint, 0, 0, 0]); // cape: one joint
        bin.extend_from_slice(&f32s(&[1.0, 0.0, 0.0, 0.0]));
        bin
    }

    #[test]
    fn class_piece_bones_subset_of_body() {
        use budget::AssetClass::Hero;
        // Cape on a body bone: clean.
        let r = classed(&pack_bin(&piece_doc(), &piece_bin(1)), Hero);
        assert!(r.diags.is_empty(), "unexpected: {:?}", diags_str(&r));
        assert!(
            warn_for(&r, "P_PIECE_BONES").is_none(),
            "body-only cape must pass"
        );
        // Cape on DEF-d, which carries no body weight: warn, name it.
        let r = classed(&pack_bin(&piece_doc(), &piece_bin(3)), Hero);
        assert!(r.diags.is_empty(), "weights are valid: {:?}", diags_str(&r));
        let d = warn_for(&r, "P_PIECE_BONES").expect("missing P_PIECE_BONES");
        assert!(d.contains("Cape"), "no mesh: {d}");
        assert!(d.contains("1 non-body bone"), "no count: {d}");
        assert!(d.contains("DEF-d"), "no bone name: {d}");
    }

    #[test]
    fn class_piece_without_body_skips() {
        use budget::AssetClass::Hero;
        // A lone cape file has no body to compare against: silent.
        let doc = json!({
            "nodes": [{"name": "DEF-a"}, {"name": "Cape", "mesh": 0, "skin": 0}],
            "meshes": [{"name": "Cape",
                        "primitives": [{"attributes": {"JOINTS_0": 0, "WEIGHTS_0": 1}}]}],
            "skins": [{"joints": [0]}],
            "accessors": [
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 1},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 1},
            ],
            "bufferViews": [
                {"byteOffset": 0, "byteLength": 4},
                {"byteOffset": 4, "byteLength": 16},
            ],
            "buffers": [{"byteLength": 20}],
        });
        let mut bin = vec![0, 0, 0, 0];
        bin.extend_from_slice(&f32s(&[1.0, 0.0, 0.0, 0.0]));
        let r = classed(&pack_bin(&doc, &bin), Hero);
        assert!(r.diags.is_empty(), "unexpected: {:?}", diags_str(&r));
        assert!(warn_for(&r, "P_PIECE_BONES").is_none());
    }

    #[test]
    fn class_weapon_shape() {
        use budget::AssetClass::Weapon;
        // Unrigged, origin at the grip: the weapon shape, fully clean.
        // No grip node needed (the Godot-era ATTACH-* rule is gone).
        let sword = json!({
            "nodes": [{"name": "Sword", "mesh": 0}],
            "meshes": [{"name": "Sword", "primitives": [{}]}],
        });
        let r = classed(&pack(&sword), Weapon);
        assert!(
            r.diags.is_empty(),
            "weapon may be unrigged: {:?}",
            diags_str(&r)
        );
        assert!(r.warnings().is_empty(), "clean weapon warns nothing");
        assert_eq!(r.summary, "0 joints, 1 mesh, 0 clips, class weapon");
        // A skin on a weapon warns (and the non-DEF joint fails).
        let mut skinned = sword.clone();
        skinned["skins"] = json!([{"joints": [0]}]);
        let r = classed(&pack(&skinned), Weapon);
        assert!(codes(&r).contains(&"R_JOINT_PREFIX"));
        let d = warn_for(&r, "P_WEAPON_SKIN").expect("missing P_WEAPON_SKIN");
        assert!(d.contains("1 skin"), "no count: {d}");
        assert!(warn_for(&r, "P_WEAPON_ATTACH").is_none());
    }

    #[test]
    fn class_hero_sockets() {
        use budget::AssetClass::*;
        // Socket under the hand bone: clean.
        let good = json!({
            "nodes": [{"name": "DEF-hand.R", "children": [1]}, {"name": "Socket_Hand_R"}],
            "skins": [{"joints": [0]}],
            "materials": [{"normalTexture": {"index": 0}}],
        });
        let r = classed(&pack(&good), Hero);
        assert!(
            r.warnings().is_empty(),
            "{:?}",
            r.warnings().iter().map(|w| &w.detail).collect::<Vec<_>>()
        );
        // Rigged hero with no hand socket.
        let mut none = good.clone();
        none["nodes"] = json!([{"name": "DEF-hand.R"}]);
        let r = classed(&pack(&none), Hero);
        let d = warn_for(&r, "P_SOCKET").expect("missing P_SOCKET");
        assert!(d.contains("no Socket_Hand_R node"), "{d}");
        // NPCs need not carry one; unrigged hero blockouts skip.
        assert!(warn_for(&classed(&pack(&none), Npc), "P_SOCKET").is_none());
        let blockout = json!({"nodes": [{"name": "Body", "mesh": 0}],
                              "meshes": [{"primitives": [{}]}]});
        assert!(warn_for(&classed(&pack(&blockout), Hero), "P_SOCKET").is_none());
        // A socket left at the root, and a socket that is a joint.
        let loose = json!({
            "nodes": [{"name": "DEF-hand.R"}, {"name": "Socket_Hand_R"}, {"name": "Socket_Back"}],
            "skins": [{"joints": [0, 2]}],
            "materials": [{"normalTexture": {"index": 0}}],
        });
        let r = classed(&pack(&loose), Hero);
        let w: Vec<&str> = r
            .warnings()
            .iter()
            .filter(|w| w.code == "P_SOCKET")
            .map(|w| w.detail.as_str())
            .collect();
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(
            w[0].contains("'Socket_Hand_R' has no bone ancestor"),
            "{w:?}"
        );
        assert!(w[1].contains("'Socket_Back' is a skin joint"), "{w:?}");
    }

    #[test]
    fn lod_far_mesh_must_be_cheaper() {
        let marker = |near: bool| json!({"skein": [{"hll::level::RenderLod": {"near": near}}]});
        let doc = |far_idx: u64| {
            json!({
                "nodes": [
                    {"name": "Rock", "mesh": 0, "extras": marker(true)},
                    {"name": "Rock_LOD1", "mesh": 1, "extras": marker(false)},
                ],
                "meshes": [
                    {"primitives": [{"attributes": {"POSITION": 0}, "indices": 1}]},
                    {"primitives": [{"attributes": {"POSITION": 0}, "indices": 2}]},
                ],
                "accessors": [{"count": 3}, {"count": 300}, {"count": far_idx}],
            })
        };
        let mobile = budget::Budget::default();
        let r = check_glb_with_budget(&pack(&doc(105)), Some(&mobile));
        assert!(warn_for(&r, "P_LOD_TRIS").is_none());
        let r = check_glb_with_budget(&pack(&doc(300)), Some(&mobile));
        let d = warn_for(&r, "P_LOD_TRIS").expect("missing P_LOD_TRIS");
        assert!(
            d.contains("'Rock_LOD1': 100 tris, not fewer than the near mesh's 100"),
            "{d}"
        );
    }

    #[test]
    fn class_level_unrigged_with_lods() {
        use budget::AssetClass::Level;
        let marker = |near: bool| json!({"skein": [{"hll::level::RenderLod": {"near": near}}]});
        let doc = json!({
            "nodes": [
                {"name": "Rock", "mesh": 0, "extras": marker(true)},
                {"name": "Rock_LOD1", "mesh": 0, "extras": marker(false)},
                {"name": "Tree", "mesh": 0, "extras": marker(true)},
            ],
            "scenes": [{"nodes": [0, 1, 2]}],
            "meshes": [{"primitives": [{}]}],
        });
        let r = classed(&pack(&doc), Level);
        // Unrigged is fine; the unpaired near mesh fails.
        assert_eq!(codes(&r), vec!["L_LOD_PAIR"]);
        assert!(r.summary.contains("class level"), "{}", r.summary);
        assert_eq!(budget::Budget::mobile_for(Level).max_tris_per_mesh, 100_000);
    }

    #[test]
    fn class_prop_relaxes_skeleton() {
        use budget::AssetClass::*;
        let doc = json!({
            "nodes": [{"name": "Crate", "mesh": 0}],
            "scenes": [{"nodes": [0]}],
            "meshes": [{"name": "Crate", "primitives": [{}]}],
            "animations": [{
                "channels": [{"target": {"node": 0}}],
                "samplers": [{}],
            }],
        });
        let bytes = pack(&doc);
        // Props may be unrigged, anims included.
        let r = classed(&bytes, Prop);
        assert!(r.diags.is_empty(), "prop relaxes: {:?}", diags_str(&r));
        // Characters and unclassed runs still require a skeleton.
        let r = classed(&bytes, Npc);
        assert!(codes(&r).contains(&"R_NO_SKIN"));
        assert!(codes(&r).contains(&"R_ANIM_TARGET"));
        let r = check_glb(&bytes);
        assert!(codes(&r).contains(&"R_NO_SKIN"));
        // A rigged prop still faces the full contract.
        let mut rigged = doc.clone();
        rigged["nodes"][0]["skin"] = json!(0);
        rigged["skins"] = json!([{"joints": [0]}]);
        let r = classed(&pack(&rigged), Prop);
        assert!(codes(&r).contains(&"R_JOINT_PREFIX"));
    }

    #[test]
    fn defects_morph_clean_passes() {
        // Matching weights/targets, matching counts, and a weights-less
        // primitive (defaults are zeros): all valid, all silent.
        let doc = json!({
            "nodes": [{"name": "DEF-a"}],
            "skins": [{"joints": [0]}],
            "meshes": [
                {"name": "A", "weights": [0.5],
                 "primitives": [{
                     "attributes": {"POSITION": 0},
                     "targets": [{"POSITION": 1}],
                 }]},
                {"name": "B",
                 "primitives": [{
                     "attributes": {"POSITION": 0},
                     "targets": [{"POSITION": 1}],
                 }]},
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "type": "VEC3", "count": 2},
                {"bufferView": 1, "componentType": 5126, "type": "VEC3", "count": 2},
            ],
            "bufferViews": [
                {"byteOffset": 0, "byteLength": 24},
                {"byteOffset": 24, "byteLength": 24},
            ],
            "buffers": [{"byteLength": 48}],
        });
        let r = check_glb(&pack_bin(&doc, &[0u8; 48]));
        assert!(r.diags.is_empty(), "unexpected: {:?}", diags_str(&r));
    }
}
