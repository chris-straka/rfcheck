# rfcheck

Rig-contract checker for game-bound GLBs. Blender (rigforge) makes the
GLBs; rfcheck verifies them — no Blender needed, so it runs in CI, in
tests, and against any rigger's output (hello, bake-off).

```sh
cargo build --release
./target/release/rfcheck hero.glb stalker.glb
./target/release/rfcheck --json hero.glb   # machine-readable
./target/release/rfcheck --mobile hero.glb # + perf-budget warnings
./target/release/rfcheck --budget phone.toml hero.glb
```

Output is one `CODE path: detail` line per finding, `OK path: summary`
when clean. Exit 0 = clean, 1 = contract failures, 2 = usage/IO error
(same shape as `animforge/dcc/export_check.py`, deliberately).
Budget (`P_*`) findings are warnings: they print like findings but
never fail, so exit stays 0 unless a contract layer also fires.

## Contract (v3)

A game-bound rig GLB must:

- parse as a glTF 2.0 binary container,
- contain a skeleton (`skins`, non-empty joints),
- have every joint named `DEF-*` (no control/mechanism leaks),
- have every animation channel target a `DEF-*` joint,
- blend at most 4 joints per skinned vertex (one
  `JOINTS`/`WEIGHTS` set),
- have skin weights summing to 1.0 (±0.001) on every skinned
  vertex,
- store skinning accessors dense (non-sparse) and well-formed
  (`JOINTS_n` as `VEC4` `UNSIGNED_BYTE`/`SHORT`, `WEIGHTS_n` as
  `VEC4` `FLOAT` or normalized `UNSIGNED_BYTE`/`SHORT`),
- be structurally sound: every accessor byte range inside its
  bufferView and buffer, finite animation outputs, keyframed
  sampler inputs, in-range joint indices, non-empty geometry,
  and agreeing morph-target counts (the `X_*` layer below —
  objective defects, zero false positives by construction).

Skeleton-only exports (no meshes) are valid. Clean files report
`max N infl/vert` in the `OK` summary.

## Codes

| Code | Meaning |
| --- | --- |
| `D_GLB_MAGIC` | not a glTF binary container |
| `D_GLB_VERSION` | container version != 2 |
| `D_GLB_TRUNC` | declared/chunk length exceeds file |
| `D_GLB_JSON` | JSON chunk missing or undecodable |
| `R_IO` | file unreadable |
| `R_NO_SKIN` | no skins in file |
| `R_NO_JOINTS` | skin has no joints |
| `R_JOINT_INDEX` | joint references missing node |
| `R_JOINT_PREFIX` | non-`DEF-` joint exported |
| `R_ANIM_TARGET` | animation targets non-`DEF-` node |
| `W_OVER_INFLUENCE` | vertex blends more than 4 joints |
| `W_UNNORMALIZED` | vertex weights do not sum to 1.0 |
| `W_SPARSE` | skinning accessor is sparse (unchecked) |
| `W_BAD_ACCESSOR` | skinning accessor unreadable/malformed |
| `X_ACCESSOR_BOUNDS` | accessor byte range exceeds bufferView/buffer |
| `X_ANIM_KEYS` | sampler input has < 1 keyframes |
| `X_ANIM_NAN` | animation output contains NaN/Inf floats |
| `X_JOINT_RANGE` | `JOINTS_n` index >= skin joint count |
| `X_EMPTY_MESH` | mesh with no primitives or zero vertices |
| `X_EMPTY_PRIM` | primitive with zero vertices and zero indices |
| `X_MORPH_COUNT` | morph weights/targets/counts disagree |
| `P_VERTS` | mesh vertex count over budget (warn) |
| `P_TRIS` | mesh triangle count over budget (warn) |
| `P_TEX_SIZE` | texture dims over budget (warn) |
| `P_INFLUENCES` | max influences/vert over budget (warn) |

`D_*` codes mirror `animforge/dcc/export_check.py`, whose container
checks are deliberately duplicated here (asset hygiene vs rig
contract are different layers; this tool owns its own parsing).

`X_*` is the objective-defect layer: each code fires only on data
that is provably broken regardless of rig style or budget — no
thresholds, no opinions. Anything this layer cannot prove (dangling
references, unknown component types, sparse storage) is skipped
silently; a bounds failure on a skinning accessor may additionally
surface as `W_BAD_ACCESSOR`, since the weight layer owns its own
reads.

## Perf budgets (`P_*`, opt-in)

`--mobile` checks the file against phone-class perf budgets. These
are WARN-level by design: a mesh that passes every contract check
but blows the triangle budget warns, it does not fail — correctness
stays FAIL-level, budgets answer "will this hurt on a phone". Plain
`rfcheck` without flags runs no budget checks at all (v0.3.0
behavior, byte-identical output). `--json` gains a `"warns"` array
only when budget checks ran.

Defaults target a 2024 flagship (Samsung Galaxy S24 class floor):

| Key | Default | Rationale |
| --- | --- | --- |
| `max_tris_per_mesh` | 100,000 | engines ship mobile heroes at 50–100k tris per character draw (Unity URP / UE5 mobile guidance) |
| `max_verts_per_mesh` | 65,535 | order of the 16-bit index ceiling: past this a mesh cannot draw in one UINT16-indexed call, so engines split prims or widen to 32-bit indices — both cost on tile-based GPUs |
| `max_texture_dim` | 2048 | largest single texture on a mobile hero; a 4k RGBA costs 16 MB even ASTC-compressed, and 2k is standard phone practice |
| `max_influences` | 4 | matches the rig contract: 4-bone skinning is the mobile GPU standard; tighten for low-end targets |

Measurement notes: verts/tris are per mesh (summed over
primitives, from accessor counts); tris count triangle lists only
(points/lines/other modes carry no triangle-list cost and are
excluded). Texture dims are sniffed from embedded PNG/JPEG/KTX2
headers — glTF JSON carries no width/height — so external/data-URI
images and foreign codecs are skipped silently (unmeasured, not
over-budget). `P_INFLUENCES` reuses the weight layer's stats, so
prims it could not read contribute nothing here either. Like `X_*`,
a mesh warns only on provable counts: unknown prims are noted
(`+N uncounted`) and can only add, so a warn on partial data is
still sound.

`--budget <file>` overrides any subset of the defaults (missing
keys keep mobile defaults; unknown keys are an error, so typos fail
loudly). Format is sniffed from content — a leading `{` means JSON,
anything else the flat TOML subset (`key = value` lines, `#`
comments, no sections):

```json
{"max_tris_per_mesh": 50000, "max_texture_dim": 1024}
```

```toml
# low-end phone profile
max_tris_per_mesh = 50000
max_verts_per_mesh = 32767
max_texture_dim = 1024
max_influences = 2
```

`--budget` implies budget checks; `--mobile --budget file` is the
same run spelled explicitly.

## Roadmap

- v2: done — BIN weight stats (max influences per vertex,
  unnormalized detection) via accessor parsing.
- v3: done — `X_*` objective-defect layer (accessor bounds,
  animation NaN/keyframes, joint ranges, empty geometry,
  morph counts).
- v4: done — `P_*` perf-budget layer (`--mobile`, `--budget`;
  per-mesh verts/tris, texture dims, influences; WARN-level).
- HLL CI wiring: done — `validate_assets.py` shells out for inbound
  character/creature GLBs, and HLL's CI installs rfcheck via cargo.

License: MIT.
