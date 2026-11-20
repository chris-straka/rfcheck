// SPDX-License-Identifier: MIT
//! Gate tests for rfcheck-6: per-game `budget.toml` schema, discovery,
//! backwards-compatible defaults, and loud missing/malformed errors.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_rfcheck");

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rfcheck_budget_toml_{tag}_{}_{}",
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn pack_glb(doc: &Value, bin: &[u8]) -> Vec<u8> {
    let mut j = serde_json::to_vec(doc).expect("serialize gltf json");
    while !j.len().is_multiple_of(4) {
        j.push(b' ');
    }
    let mut b = bin.to_vec();
    while !b.len().is_multiple_of(4) {
        b.push(0);
    }
    let total = 12 + 8 + j.len() + if b.is_empty() { 0 } else { 8 + b.len() };
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&0x46546C67u32.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(j.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x4E4F534Au32.to_le_bytes());
    out.extend_from_slice(&j);
    if !b.is_empty() {
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x004E4942u32.to_le_bytes());
        out.extend_from_slice(&b);
    }
    out
}

/// Synthetic rigged monster GLB with exact triangle count and 10,000 vertices
/// (under the 12,000 monster vertex cap so tests isolate `P_TRIS`).
fn write_monster_glb(path: &Path, tris: usize) {
    let verts: usize = 10_000;
    let idx: usize = tris * 3;
    let pv = verts * 12;
    let ib = idx * 2;
    let doc = json!({
        "asset": {"version": "2.0"},
        "nodes": [{"name": "DEF-root"}],
        "skins": [{"joints": [0]}],
        "materials": [{"normalTexture": {"index": 0}}],
        "meshes": [{"name": "Monster", "primitives": [
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
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create glb parent dir");
    }
    fs::write(path, pack_glb(&doc, &vec![0u8; pv + ib])).expect("write glb");
}

const SAMPLE_GAME_BUDGET_TOML: &str = "\
# Per-game asset budget (schema v1)
schema = 1

[class.hero]
max_tris_per_mesh = 25_000
max_verts_per_mesh = 16_667
max_texture_dim = 1024
max_influences = 4
max_bones = 160

[class.npc]
max_tris_per_mesh = 8_000
max_verts_per_mesh = 6_000
max_texture_dim = 512
max_influences = 4
max_bones = 65

[class.monster]
max_tris_per_mesh = 18_000
max_verts_per_mesh = 12_000
max_texture_dim = 1024
max_influences = 4
max_bones = 128

[class.prop]
max_tris_per_mesh = 2_000
max_verts_per_mesh = 1_500
max_texture_dim = 512
max_influences = 4
max_bones = 64

[class.weapon]
max_tris_per_mesh = 2_000
max_verts_per_mesh = 1_500
max_texture_dim = 512
max_influences = 4
max_bones = 16

[class.level]
max_tris_per_mesh = 100_000
max_verts_per_mesh = 65_535
max_texture_dim = 2048
max_influences = 4
max_bones = 256
";

fn run_rfcheck(args: &[&str], cwd: Option<&Path>) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.output().expect("spawn rfcheck")
}

fn parse_stdout_json(out: &Output) -> Value {
    let s = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&s).unwrap_or_else(|e| {
        panic!(
            "expected JSON stdout, got error {e}\nstatus: {:?}\nstdout: {s}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn warns_with_code<'a>(report: &'a Value, code: &str) -> Vec<&'a str> {
    report
        .get("warns")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter(|w| w.get("code").and_then(Value::as_str) == Some(code))
                .filter_map(|w| w.get("detail").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn game_budget_toml_monster_18001_warns_and_18000_passes() {
    let tmp = TempDir::new("monster_18001");
    let budget_file = tmp.path().join("budget.toml");
    fs::write(&budget_file, SAMPLE_GAME_BUDGET_TOML).expect("write budget.toml");

    let over_glb = tmp.path().join("monster_18001.glb");
    let at_cap_glb = tmp.path().join("monster_18000.glb");
    write_monster_glb(&over_glb, 18_001);
    write_monster_glb(&at_cap_glb, 18_000);

    // 18,001 triangles MUST warn P_TRIS against the game's 18,000 monster cap.
    let out = run_rfcheck(
        &[
            "--json",
            "--budget",
            budget_file.to_str().unwrap(),
            "--class",
            "monster",
            over_glb.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "P_TRIS is warn-only (exit 0); stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = parse_stdout_json(&out);
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["class"], json!("monster"));
    assert_eq!(r["diags"], json!([]));
    let p_tris = warns_with_code(&r, "P_TRIS");
    assert_eq!(p_tris.len(), 1, "expected P_TRIS warning, got: {r}");
    assert!(
        p_tris[0].contains("18001 tris exceed 18000 monster budget"),
        "unexpected P_TRIS detail: {}",
        p_tris[0]
    );

    // 18,000 triangles MUST NOT warn P_TRIS.
    let out = run_rfcheck(
        &[
            "--json",
            "--budget",
            budget_file.to_str().unwrap(),
            "--class",
            "monster",
            at_cap_glb.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = parse_stdout_json(&out);
    assert_eq!(r["ok"], json!(true));
    assert!(
        warns_with_code(&r, "P_TRIS").is_empty(),
        "18,000 tris must be within 18,000 cap: {r}"
    );
}

#[test]
fn game_budget_dir_argument_and_print_budget() {
    let tmp = TempDir::new("dir_arg");
    let game_dir = tmp.path().join("my_game");
    fs::create_dir_all(&game_dir).expect("create game dir");
    let budget_file = game_dir.join("budget.toml");
    fs::write(&budget_file, SAMPLE_GAME_BUDGET_TOML).expect("write budget.toml");

    let over_glb = tmp.path().join("outside_18001.glb");
    write_monster_glb(&over_glb, 18_001);

    // Passing the game directory itself to --budget resolves <dir>/budget.toml.
    let out = run_rfcheck(
        &[
            "--json",
            "--budget",
            game_dir.to_str().unwrap(),
            "--class",
            "monster",
            over_glb.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = parse_stdout_json(&out);
    let p_tris = warns_with_code(&r, "P_TRIS");
    assert_eq!(p_tris.len(), 1, "expected P_TRIS via dir --budget: {r}");
    assert!(
        p_tris[0].contains("18001 tris exceed 18000 monster budget"),
        "unexpected detail: {}",
        p_tris[0]
    );

    // --print-budget accepts either <game>/budget.toml or <game> directory.
    for arg in [budget_file.to_str().unwrap(), game_dir.to_str().unwrap()] {
        let out = run_rfcheck(
            &["--print-budget", "--budget", arg, "--class", "monster"],
            None,
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "print-budget failed for {arg}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let j = parse_stdout_json(&out);
        assert_eq!(j["class"], json!("monster"));
        assert_eq!(j["max_tris_per_mesh"], json!(18_000));
        assert_eq!(j["max_verts_per_mesh"], json!(12_000));
        assert_eq!(j["max_texture_dim"], json!(1024));
        assert_eq!(j["max_influences"], json!(4));
        assert_eq!(j["max_bones"], json!(128));
    }
}

#[test]
fn game_budget_auto_discovers_ancestor_budget_toml() {
    let tmp = TempDir::new("autodiscover");
    let game_dir = tmp.path().join("game_a");
    fs::create_dir_all(&game_dir).expect("create game dir");
    fs::write(game_dir.join("budget.toml"), SAMPLE_GAME_BUDGET_TOML).expect("write budget.toml");

    let glb = game_dir.join("enemies/stalker/models/glb/stalker.glb");
    write_monster_glb(&glb, 18_001);

    // Checking a GLB inside a game dir with --class monster (no --budget flag)
    // automatically reads <game>/budget.toml.
    let out = run_rfcheck(
        &["--json", "--class", "monster", glb.to_str().unwrap()],
        None,
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = parse_stdout_json(&out);
    let p_tris = warns_with_code(&r, "P_TRIS");
    assert_eq!(
        p_tris.len(),
        1,
        "expected auto-discovered P_TRIS warning: {r}"
    );
    assert!(
        p_tris[0].contains("18001 tris exceed 18000 monster budget"),
        "unexpected detail: {}",
        p_tris[0]
    );

    // --print-budget inside a game subdirectory discovers <game>/budget.toml.
    let sub = game_dir.join("enemies/stalker");
    let out = run_rfcheck(&["--print-budget", "--class", "monster"], Some(&sub));
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j = parse_stdout_json(&out);
    assert_eq!(j["max_tris_per_mesh"], json!(18_000));

    // Plain rfcheck (no --class, --mobile, or --budget) stays backwards-compatible:
    // no budget warnings are added even when budget.toml exists above the GLB.
    let out = run_rfcheck(&["--json", glb.to_str().unwrap()], None);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = parse_stdout_json(&out);
    assert!(r.get("warns").is_none(), "plain run must omit warns: {r}");
}

#[test]
fn game_budget_stops_at_game_root_and_keeps_backwards_compatible_defaults() {
    let tmp = TempDir::new("defaults_boundary");
    // Outer folder has a budget.toml, but inner game has gamekit.toml and no budget.toml.
    fs::write(tmp.path().join("budget.toml"), SAMPLE_GAME_BUDGET_TOML).expect("write outer budget");
    let other_game = tmp.path().join("other_game");
    fs::create_dir_all(&other_game).expect("create other_game");
    fs::write(
        other_game.join("gamekit.toml"),
        "[game]\nname = \"Other\"\n",
    )
    .expect("write gamekit.toml");

    let glb_18001 = other_game.join("enemies/beast/models/glb/beast_18001.glb");
    let glb_20001 = other_game.join("enemies/beast/models/glb/beast_20001.glb");
    write_monster_glb(&glb_18001, 18_001);
    write_monster_glb(&glb_20001, 20_001);

    // Must NOT inherit outer budget.toml past other_game/gamekit.toml; built-in
    // monster default (20,000) applies so 18,001 passes and 20,001 warns.
    let out = run_rfcheck(
        &["--json", "--class", "monster", glb_18001.to_str().unwrap()],
        None,
    );
    assert_eq!(out.status.code(), Some(0));
    let r = parse_stdout_json(&out);
    assert!(
        warns_with_code(&r, "P_TRIS").is_empty(),
        "must stop at gamekit.toml and use 20,000 default: {r}"
    );

    let out = run_rfcheck(
        &["--json", "--class", "monster", glb_20001.to_str().unwrap()],
        None,
    );
    assert_eq!(out.status.code(), Some(0));
    let r = parse_stdout_json(&out);
    let p_tris = warns_with_code(&r, "P_TRIS");
    assert_eq!(p_tris.len(), 1);
    assert!(
        p_tris[0].contains("20001 tris exceed 20000 monster budget"),
        "unexpected default detail: {}",
        p_tris[0]
    );

    // --print-budget inside other_game returns built-in 20,000 default.
    let out = run_rfcheck(&["--print-budget", "--class", "monster"], Some(&other_game));
    assert_eq!(out.status.code(), Some(0));
    let j = parse_stdout_json(&out);
    assert_eq!(j["max_tris_per_mesh"], json!(20_000));

    // Legacy flat TOML (no schema key) and legacy JSON still work unchanged.
    let legacy_toml = tmp.path().join("legacy.toml");
    fs::write(&legacy_toml, "max_tris_per_mesh = 18_000\n").expect("write legacy toml");
    let out = run_rfcheck(
        &[
            "--json",
            "--budget",
            legacy_toml.to_str().unwrap(),
            "--class",
            "monster",
            glb_18001.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(0));
    let r = parse_stdout_json(&out);
    assert_eq!(warns_with_code(&r, "P_TRIS").len(), 1);

    let legacy_json = tmp.path().join("legacy.json");
    fs::write(
        &legacy_json,
        r#"{"class": {"monster": {"max_tris_per_mesh": 18000}}}"#,
    )
    .expect("write legacy json");
    let out = run_rfcheck(
        &[
            "--json",
            "--budget",
            legacy_json.to_str().unwrap(),
            "--class",
            "monster",
            glb_18001.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(0));
    let r = parse_stdout_json(&out);
    assert_eq!(warns_with_code(&r, "P_TRIS").len(), 1);
}

#[test]
fn game_budget_missing_and_malformed_fail_exit_2() {
    let tmp = TempDir::new("missing_malformed");
    let glb = tmp.path().join("m.glb");
    write_monster_glb(&glb, 18_001);

    // 1. Missing explicit budget file -> exit 2.
    let missing = tmp.path().join("does_not_exist/budget.toml");
    let out = run_rfcheck(
        &[
            "--budget",
            missing.to_str().unwrap(),
            "--class",
            "monster",
            glb.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(2), "missing file must exit 2");
    assert!(
        !out.stderr.is_empty(),
        "missing file must explain error on stderr"
    );

    // 2. Explicit directory with no budget.toml -> exit 2.
    let empty_dir = tmp.path().join("empty_game");
    fs::create_dir_all(&empty_dir).expect("create empty_dir");
    let out = run_rfcheck(
        &[
            "--budget",
            empty_dir.to_str().unwrap(),
            "--class",
            "monster",
            glb.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "dir without budget.toml must exit 2"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("budget.toml"),
        "expected budget.toml in stderr, got: {stderr}"
    );

    // 3. Missing budget with --print-budget -> exit 2.
    let out = run_rfcheck(
        &["--print-budget", "--budget", missing.to_str().unwrap()],
        None,
    );
    assert_eq!(out.status.code(), Some(2));

    // 4. Malformed budget.toml cases -> exit 2.
    let bad_cases = [
        ("schema_v2", "schema = 2\n[class.monster]\nmax_tris_per_mesh = 18_000\n"),
        ("schema_v0", "schema = 0\n"),
        ("schema_in_section", "[class.monster]\nschema = 1\nmax_tris_per_mesh = 18_000\n"),
        (
            "dup_key_section",
            "schema = 1\n[class.monster]\nmax_tris_per_mesh = 18_000\nmax_tris_per_mesh = 20_000\n",
        ),
        (
            "dup_key_top",
            "max_tris_per_mesh = 18_000\nmax_tris_per_mesh = 20_000\n",
        ),
        (
            "dup_section",
            "schema = 1\n[class.monster]\nmax_tris_per_mesh = 18_000\n[class.monster]\nmax_bones = 64\n",
        ),
        ("bad_section", "[monster]\nmax_tris_per_mesh = 18_000\n"),
        ("bad_key", "schema = 1\n[class.monster]\nmax_tris = 18_000\n"),
        ("bad_value", "schema = 1\n[class.monster]\nmax_tris_per_mesh = many\n"),
    ];
    for (name, content) in bad_cases {
        let p = tmp.path().join(format!("bad_{name}.toml"));
        fs::write(&p, content).expect("write bad budget");
        let out = run_rfcheck(
            &[
                "--budget",
                p.to_str().unwrap(),
                "--class",
                "monster",
                glb.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(
            out.status.code(),
            Some(2),
            "malformed case '{name}' must exit 2; stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // 5. Malformed auto-discovered budget.toml MUST fail loudly (exit 2), never silently fallback.
    let broken_game = tmp.path().join("broken_game");
    fs::create_dir_all(&broken_game).expect("create broken_game");
    fs::write(broken_game.join("budget.toml"), "schema = 99\n").expect("write broken budget");
    let broken_glb = broken_game.join("enemies/ogre/models/glb/ogre.glb");
    write_monster_glb(&broken_glb, 18_001);
    let out = run_rfcheck(
        &["--json", "--class", "monster", broken_glb.to_str().unwrap()],
        None,
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "malformed auto-discovered budget.toml must exit 2; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
