// SPDX-License-Identifier: MIT
//! Rig-contract checks over a parsed GLB document.

use crate::glb;
use serde_json::{json, Value};

pub struct Diag {
    pub code: &'static str,
    pub detail: String,
}

pub struct Report {
    pub diags: Vec<Diag>,
    pub summary: String,
}

impl Report {
    pub fn failed(&self) -> bool {
        !self.diags.is_empty()
    }

    pub fn to_json(&self, path: &std::path::Path) -> String {
        let diags: Vec<Value> = self
            .diags
            .iter()
            .map(|d| json!({"code": d.code, "detail": d.detail}))
            .collect();
        json!({
            "file": path.display().to_string(),
            "ok": !self.failed(),
            "summary": self.summary,
            "diags": diags,
        })
        .to_string()
    }
}

fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn node_name(nodes: &[Value], idx: usize) -> Option<&str> {
    nodes
        .get(idx)
        .and_then(|n| n.get("name"))
        .and_then(Value::as_str)
}

fn join_names(names: &[String]) -> String {
    const CAP: usize = 5;
    if names.len() <= CAP {
        names.join(", ")
    } else {
        format!("{} (+{} more)", names[..CAP].join(", "), names.len() - CAP)
    }
}

pub fn check_glb(bytes: &[u8]) -> Report {
    let doc = match glb::parse(bytes) {
        Ok(d) => d,
        Err((code, detail)) => {
            return Report {
                diags: vec![Diag { code, detail }],
                summary: "invalid container".to_string(),
            };
        }
    };
    let json = &doc.json;
    let nodes = arr(json, "nodes");
    let skins = arr(json, "skins");
    let n_meshes = arr(json, "meshes").len();
    let clips = arr(json, "animations");

    let mut diags: Vec<Diag> = Vec::new();

    if skins.is_empty() {
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
        .filter(|n| !n.starts_with("DEF-"))
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
                Some(n) if n.starts_with("DEF-") => {}
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

    let summary = format!(
        "{} joint{}, {} mesh{}, {} clip{}",
        joints.len(),
        if joints.len() == 1 { "" } else { "s" },
        n_meshes,
        if n_meshes == 1 { "" } else { "es" },
        clips.len(),
        if clips.len() == 1 { "" } else { "s" },
    );
    Report { diags, summary }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn clean_rig_passes() {
        let doc = json!({
            "nodes": [{"name": "DEF-spine"}, {"name": "Cube", "mesh": 0}],
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
}
