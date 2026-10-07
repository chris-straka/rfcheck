# rfcheck

Rig-contract checker for game-bound GLBs. Blender (rigforge) makes the
GLBs; rfcheck verifies them — no Blender needed, so it runs in CI, in
tests, and against any rigger's output (hello, bake-off). The games are
Bevy 0.19, so it also checks what Bevy's glTF loader needs (`B_*`), HLL's
distant-mesh markers (`L_*`), and attachment sockets.

```sh
cargo build --release
./target/release/rfcheck hero.glb stalker.glb
./target/release/rfcheck --json hero.glb   # machine-readable
./target/release/rfcheck --mobile hero.glb # + perf-budget warnings
./target/release/rfcheck --budget phone.toml hero.glb
./target/release/rfcheck --class hero hero.glb   # per-class budgets
./target/release/rfcheck --class weapon sword.glb # weapons may be unrigged
./target/release/rfcheck --class level arena.glb  # levels: LOD markers, no rig
./target/release/rfcheck --image-codecs png,ktx2,jpeg ai.glb # game enables jpeg
```

Output is one `CODE path: detail` line per finding, `OK path: summary`
when clean. Exit 0 = clean, 1 = contract failures, 2 = usage/IO error
(same shape as `animforge/dcc/export_check.py`, deliberately).
Budget (`P_*`) findings are warnings: they print like findings but
never fail, so exit stays 0 unless a contract layer also fires.

GLB parsing and the shared rig rules live in [`glbkit/`](glbkit/README.md),
a zero-dependency crate that weightforge and wrapforge also use.

## Contract (v6)

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
  objective defects, zero false positives by construction),
- load whole in Bevy 0.19.1's glTF loader (the `B_*` layer below),
- pair every `RenderLod` marker (`L_LOD_PAIR`, below).

Skeleton-only exports (no meshes) are valid. Clean files report
`max N infl/vert` in the `OK` summary.

`--class prop`, `--class weapon`, and `--class level` relax the
skeleton requirement: static props, weapons, and levels are
legitimately unrigged, so an unrigged file passes (animation targets go unchecked with it — there is no
DEF rule to check them against). A rigged prop still faces the full
contract above.

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
| `B_EXT_REQUIRED` | `extensionsRequired` names an extension Bevy cannot load (Draco, meshopt, quantization, basisu, ...) |
| `B_PRIM_MODE` | primitive mode LINE_LOOP or TRIANGLE_FAN (Bevy rejects) |
| `B_NODE_CYCLE` | node hierarchy has a cycle (Bevy needs a tree) |
| `B_IMAGE_CODEC` | used texture in a codec the game's Bevy build lacks, unknown mime, or bytes that disagree with `mimeType` |
| `B_MORPH_LIMIT` | > 256 morph targets, or too many morphed verts for Bevy's 2048² morph texture |
| `B_ANIM_ORPHAN` | clip animates a node outside every scene (Bevy drops the curve) |
| `B_ANIM_PATH` | two animated nodes share a name path (one Bevy `AnimationTargetId`) |
| `B_CLIP_NAME` | duplicate clip names (`named_animations` keeps one) |
| `L_LOD_PAIR` | `RenderLod` near mesh without its `_LOD1`, far mesh without its near, or malformed marker |
| `P_VERTS` | mesh vertex count over budget (warn) |
| `P_TRIS` | mesh triangle count over budget (warn) |
| `P_TEX_SIZE` | texture dims over budget (warn) |
| `P_TEX_NORMAL` | character with no normal map (warn) |
| `P_INFLUENCES` | max influences/vert over budget (warn) |
| `P_BONES` | deform-bone count over budget (warn) |
| `P_PIECE_BONES` | cape/hair skinned to non-body bones (warn) |
| `P_WEAPON_SKIN` | weapon file carries a skin (warn) |
| `P_SOCKET` | `Socket_*` node not under a bone or itself a joint; rigged hero without `Socket_Hand_R` (warn) |
| `P_LOD_TRIS` | far `_LOD1` mesh not cheaper than its near mesh (warn) |

`D_*` codes mirror `animforge/dcc/export_check.py`, whose container
checks are deliberately duplicated here (asset hygiene vs rig
contract are different layers; this tool owns its own parsing).

`P_WEAPON_ATTACH` (Godot `BoneAttachment3D` grip node) was removed in
v0.6.0: Bevy weapons snap to the character's hand socket instead.

`X_*` is the objective-defect layer: each code fires only on data
that is provably broken regardless of rig style or budget — no
thresholds, no opinions. Anything this layer cannot prove (dangling
references, unknown component types, sparse storage) is skipped
silently; a bounds failure on a skinning accessor may additionally
surface as `W_BAD_ACCESSOR`, since the weight layer owns its own
reads.

## Bevy loader (`B_*`) and LOD markers (`L_*`)

The games load GLBs through Bevy 0.19.1's `bevy_gltf`. Every `B_*`
code is a file that loader rejects outright (the whole GLB fails) or
silently loses data from, read off its source, and each is checked
against the real loader by `examples/bevy_fixtures.rs` +
`glbkit/compat/bevy` (see "Verification" below). Like `X_*` they
fail, and fire only on what the JSON proves.

- Required extensions: Bevy's glTF build enables
  `KHR_lights_punctual`, `KHR_materials_unlit`, `KHR_texture_transform`,
  `KHR_materials_transmission`, `KHR_materials_ior`,
  `KHR_materials_emissive_strength`; anything else in
  `extensionsRequired` fails validation. Optional (`extensionsUsed`)
  extensions are ignored, not errors.
- Textures: only images a texture uses are loaded. The default codec
  set is HLL's Bevy build (`2d`/`3d`/`ui` features: `png`, `ktx2`);
  `--image-codecs png,ktx2,jpeg` widens it for a game that enables
  more. AI GLBs (Tripo) embed JPEG: re-encode them, or enable Bevy's
  `jpeg` feature in the game and pass the flag.
- Animation: Bevy targets curves by the node-name path from a scene
  root (unnamed nodes become `GltfNode{i}`), so curves on nodes
  outside every scene are dropped and same-path nodes collide; the
  game looks clips up by name, so names must be unique.
- Joint palette: Bevy's `MAX_JOINTS` is 256, the generic `max_bones`.

`L_LOD_PAIR` reads HLL's Skein marker
`{"skein": [{"hll::level::RenderLod": {"near": bool}}]}` (written by
HLL `tools/add_port_lods.py`). A near mesh `X` pairs with the far
mesh `X_LOD1`. On mobile the near one hides past 40–50 m and the far
one shows from there, so an unpaired marker is a mesh that pops out
or never hands off. Budget runs add `P_LOD_TRIS` when a far mesh is
not cheaper than its near one.

Sockets are plain nodes named `Socket_*`, parented under a bone, that
the game finds by `Name` (HLL snaps the sword to `Socket_Hand_R` every
frame). Budget runs warn (`P_SOCKET`) when a socket has no bone
ancestor or is itself a skin joint, and when a rigged hero has no
`Socket_Hand_R`. Unrigged blockout prefabs skip.

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
| `max_bones` | 256 | Bevy's `MAX_JOINTS`: 256 matrices × 64 B = 16 KB, exactly the guaranteed UBO size; past this the palette cannot upload everywhere |

## Asset classes (`--class`)

`--class hero|npc|monster|prop|weapon|level` checks the file against that
class's mobile-profile budget instead of the generic flagship one.
Class runs name the class in each warn (`exceed 15000 hero budget`),
in the summary (`, class hero`), and in JSON (`"class"` key, present
only for classed runs). `--class` implies budget checks, like
`--budget`.

| Class | Tris | Verts | Tex px | Infl | Bones |
| --- | --- | --- | --- | --- | --- |
| `hero` | 15,000 | 10,000 | 1024 | 4 | 160 |
| `npc` | 8,000 | 6,000 | 512 | 4 | 65 |
| `monster` | 20,000 | 12,000 | 1024 | 4 | 128 |
| `prop` | 2,000 | 1,500 | 512 | 4 | 64 |
| `weapon` | 2,000 | 1,500 | 512 | 4 | 16 |
| `level` | 100,000 | 65,535 | 2048 | 4 | 256 |

Heroes follow HLL art direction (5k–15k tris, 512–1024 px textures)
and allow 160 deform bones, the full rigforge hero rig with face, fingers
and twists (owner decision, 2026-10-04); NPCs get rigforge's MOBILE
profile (65 deform bones: no face, no twists; v0.6.0, was 128);
monsters may exceed heroes (one large boss draw, not a crowd); props
and weapons are small static draws; levels are many meshes, so they
keep the per-mesh flagship ceilings.

Class runs also enable convention checks (still WARN-level):

- Characters (`hero`, `npc`, `monster`) ship a baked normal map:
  `P_TEX_NORMAL` fires when no material carries `normalTexture`.
- Detachable pieces — meshes whose name contains `cape`, `cloak`,
  `hair`, `ponytail`, or `braid` — get body weights via Data
  Transfer, so `P_PIECE_BONES` fires when a piece mesh uses a bone
  that carries no body weight. Piece-only files and unreadable
  weights skip silently (nothing provable). Detection is by mesh
  name alone, so this one also runs on unclassed budget runs
  (`--mobile`, `--budget`).
- Weapons (`--class weapon` only) snap to the character's hand
  socket in the game, with their origin as the grip:
  `P_WEAPON_SKIN` fires when the file carries a skin.
- Sockets (`P_SOCKET`) and LOD triangle savings (`P_LOD_TRIS`), see
  the Bevy section above.

Measurement notes: verts/tris are per mesh (summed over
primitives, from accessor counts); tris count triangle lists only
(points/lines/other modes carry no triangle-list cost and are
excluded). Texture dims are sniffed from embedded PNG/JPEG/KTX2
headers — glTF JSON carries no width/height — so external/data-URI
images and foreign codecs are skipped silently (unmeasured, not
over-budget). `P_INFLUENCES` reuses the weight layer's stats, so
prims it could not read contribute nothing here either. `P_BONES`
counts unique `DEF-*` joint nodes across all skins (non-DEF joints
are the contract layer's business, not the budget's). Like `X_*`,
a mesh warns only on provable counts: unknown prims are noted
(`+N uncounted`) and can only add, so a warn on partial data is
still sound.

`--budget <file>` overrides any subset of the defaults (missing
keys keep mobile defaults; unknown keys are an error, so typos fail
loudly). Format is sniffed from content — a leading `{` means JSON,
anything else the TOML subset (`key = value` lines with unsigned
integers — `50_000` digit separators allowed — `#` comments, and
`[class.X]` sections):

```json
{"max_tris_per_mesh": 50000, "max_texture_dim": 1024}
```

```toml
# low-end phone profile
max_tris_per_mesh = 50000
max_verts_per_mesh = 32767
max_texture_dim = 1024
max_influences = 2
max_bones = 96
```

`--budget` implies budget checks; `--mobile --budget file` is the
same run spelled explicitly.

Per-class overrides live in `[class.X]` sections (TOML) or the
`"class"` object (JSON). Each key resolves section first, then the
file top level, then the mobile-profile default — so a flat file
keeps meaning "every asset", and a section only narrows its class:

```toml
max_tris_per_mesh = 50000

[class.hero]
max_tris_per_mesh = 15000
max_bones = 100
```

```json
{"max_tris_per_mesh": 50000,
 "class": {"hero": {"max_tris_per_mesh": 15000, "max_bones": 100}}}
```

Unknown classes, unknown keys, and non-`[class.X]` sections are all
errors.

## Roadmap

- v2: done — BIN weight stats (max influences per vertex,
  unnormalized detection) via accessor parsing.
- v3: done — `X_*` objective-defect layer (accessor bounds,
  animation NaN/keyframes, joint ranges, empty geometry,
  morph counts).
- v4: done — `P_*` perf-budget layer (`--mobile`, `--budget`;
  per-mesh verts/tris, texture dims, influences; WARN-level).
- v5: done — per-class budgets (`--class`, mobile profile per asset
  class), bone budget (`P_BONES`), normal-map (`P_TEX_NORMAL`),
  piece (`P_PIECE_BONES`), and weapon (`P_WEAPON_SKIN`,
  `P_WEAPON_ATTACH`) warnings; prop/weapon runs may be unrigged.
- v0.5.1: fixes — `--json` stays valid JSON for unreadable paths
  with `\` or `"`; budget TOML takes `50_000` and comments after
  `[class.X]`; GLB chunks must fit the header's declared length
  (`D_GLB_TRUNC`, previously a misleading `W_BAD_ACCESSOR`); the
  summary counts budget-run findings as `N warnings` (was
  `N over budget`, which miscounted convention warnings).
- v6 (0.6.0): done — Bevy re-plan. `B_*` loader layer and `L_LOD_PAIR`
  (fail), `P_SOCKET` / `P_LOD_TRIS` (warn), `--class level`,
  `--image-codecs`; NPC bones 65 (rigforge MOBILE); Godot-only
  `P_WEAPON_ATTACH` removed. Output format unchanged (same text lines,
  same JSON keys, same exit codes), so `gk validate assets` reads it
  as before.
- HLL wiring: done — `gk validate assets` (HLL's `validate_assets.py` is a
  thin wrapper) runs rfcheck on inbound character/creature GLBs when it is
  on PATH (`--rfcheck`, `--no-rfcheck`); HLL's CI no longer installs it.

## Verification

```sh
cargo run --example bevy_fixtures -- /tmp/rf_fixtures   # clean + one per B_*/L_* code
./target/release/rfcheck --class level /tmp/rf_fixtures/*.glb
cd glbkit/compat/bevy && cargo run --release --example check -- /tmp/rf_fixtures/*.glb
```

Each fixture is named after the code it should raise; Bevy must fail
the load-fatal ones and report the lost targets/clip names for the
silent ones. Last run: see `TODO.md` (2026-10-06).

License: MIT.
