// SPDX-License-Identifier: MIT
//! Small glTF JSON accessors and detail formatters shared by every
//! check layer. All lookups are total: a missing or mistyped field is
//! None (or an empty slice), never a panic.

use serde_json::Value;

/// `v[key]` as an array slice; empty when absent or not an array.
pub fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// A glTF index (non-negative integer that fits in usize).
pub fn as_idx(v: &Value) -> Option<usize> {
    v.as_u64().and_then(|n| usize::try_from(n).ok())
}

/// `v[key]` as a glTF index / count.
pub fn as_usize(v: &Value, key: &str) -> Option<usize> {
    v.get(key).and_then(as_idx)
}

/// `count` of accessor `ai`, if the accessor exists and declares one.
pub fn acc_count(json: &Value, ai: usize) -> Option<usize> {
    arr(json, "accessors")
        .get(ai)
        .and_then(|a| as_usize(a, "count"))
}

/// `mesh 3 'Body'`, or `mesh 3` when unnamed.
pub fn mesh_tag(meshes: &[Value], mi: usize) -> String {
    match meshes
        .get(mi)
        .and_then(|m| m.get("name"))
        .and_then(Value::as_str)
    {
        Some(n) if !n.is_empty() => format!("mesh {mi} '{n}'"),
        _ => format!("mesh {mi}"),
    }
}

/// Comma-joined names, capped at five with a `(+N more)` tail.
pub fn join_names(names: &[String]) -> String {
    const CAP: usize = 5;
    if names.len() <= CAP {
        names.join(", ")
    } else {
        format!("{} (+{} more)", names[..CAP].join(", "), names.len() - CAP)
    }
}

/// `one` when n == 1, else `many`.
pub fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 {
        one
    } else {
        many
    }
}
