# rfcheck

Rig-contract checker for game-bound GLBs. Blender (rigforge) makes the
GLBs; rfcheck verifies them — no Blender needed, so it runs in CI, in
tests, and against any rigger's output (hello, bake-off).

```sh
cargo build --release
./target/release/rfcheck hero.glb stalker.glb
./target/release/rfcheck --json hero.glb   # machine-readable
```

Output is one `CODE path: detail` line per finding, `OK path: summary`
when clean. Exit 0 = clean, 1 = contract failures, 2 = usage/IO error
(same shape as `animforge/dcc/export_check.py`, deliberately).

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

## Roadmap

- v2: done — BIN weight stats (max influences per vertex,
  unnormalized detection) via accessor parsing.
- v3: done — `X_*` objective-defect layer (accessor bounds,
  animation NaN/keyframes, joint ranges, empty geometry,
  morph counts).
- HLL CI wiring: done — `validate_assets.py` shells out for inbound
  character/creature GLBs, and HLL's CI installs rfcheck via cargo.

License: MIT.
