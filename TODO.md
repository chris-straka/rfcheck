
## Requested 2026-10-01 (see ~/Games/_tools/roadmap.md)

- [x] Mesh budget per asset class (hero 5k-15k tris, NPC, monster, prop)
      from the budget TOML, with a mobile profile.
      Done v0.5.0: `--class`, `[class.X]` TOML / `"class"` JSON,
      per-class mobile profile (hero 15k, npc 8k, monster 20k,
      prop/weapon 2k tris).
- [x] Texture budget: resolution per class (512-1024 px heroes), and flag
      missing normal maps on characters.
      Done v0.5.0: per-class `max_texture_dim` + `P_TEX_NORMAL`
      for hero/npc/monster runs.
- [x] Bone budget for the mobile profile (the hero rig exports 160 deform
      bones; flag over a configurable limit).
      Done v0.5.0: `max_bones` (generic 256, hero/npc/monster 128)
      counting unique DEF-* joints; the 160-bone hero warns until
      the lighter rigforge mobile profile lands.
      2026-10-04: hero raised to 160 (owner), so the full rigforge
      hero rig passes; npc/monster stay 128.
- [x] Piece checks: capes/hair skinned to body bones only; weapons have
      no skin and a named attachment point.
      Done v0.5.0: `P_PIECE_BONES` (piece-joint subset of body
      joints), `P_WEAPON_SKIN` / `P_WEAPON_ATTACH` (`ATTACH-*`
      grip node) for `--class weapon`; prop/weapon runs may be
      unrigged.

## Requested 2026-10-06: Bevy re-plan (games are Bevy 0.19)

- [x] Drop Godot-only checks: `P_WEAPON_ATTACH` (`ATTACH-*` grip node for
      Godot `BoneAttachment3D`) removed; weapons snap to the hand socket.
- [x] What Bevy's glTF loader needs (`B_*`, fail): required extensions,
      primitive modes, node cycles, texture codecs vs the game's Bevy
      features (`--image-codecs`), morph limits, animation curves Bevy
      drops (orphan nodes, same-name paths), duplicate clip names.
- [x] LOD markers: `L_LOD_PAIR` (HLL `RenderLod` near/far pairing, fail),
      `P_LOD_TRIS` (far mesh not cheaper, warn); `--class level` so
      levels can go through rfcheck unrigged.
- [x] Sockets: `P_SOCKET` (`Socket_*` under a bone, not a joint;
      rigged heroes carry `Socket_Hand_R`).
- [x] Mobile bone budgets: NPC 65 (rigforge MOBILE profile); generic 256
      is Bevy's `MAX_JOINTS`.
- [x] Output format unchanged for `gk` (same text lines, JSON keys, exit
      codes).

Verified 2026-10-06 on f-ms-7917: 86 HLL GLBs (characters by gk class,
levels/kit as `--class level`): no `B_*`/`L_*`/`P_SOCKET`/`P_LOD_TRIS`
findings; 82 RenderLod pairs all matched. Pre-existing findings
unchanged (andras_rig: bake-off bone names, 13.6k verts, 2k textures).
`gk validate assets` on HLL: output byte-identical to rfcheck 0.5.1 (as
are `--json --class hero|npc|monster` and plain runs). Every B_* rule
confirmed against Bevy's loader (glbkit/compat/README.md).

### Open

- [ ] Owner decision (Decisions, 2026-10-06): HLL's bake-off bone names
      (`Hips`, `Hand_R`, ...) vs rigforge `DEF-*`. Until then
      `andras_rig.glb` keeps `R_JOINT_PREFIX` / `R_ANIM_TARGET`.
- [ ] gk (games/_tools, not this repo): fail on `B_*` and `L_*`, not only
      `D_*` (they are load failures / vanishing meshes); map
      `levels = "level"` in `class_by_dir` so levels get the LOD checks.
- [ ] Required clip set per class (HLL needs idle/walk/run/attack_1-3/
      dodge/hit/death; the game panics on a missing one). Needs string
      lists in the budget file format.
