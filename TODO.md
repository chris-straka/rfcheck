
## Requested 2026-10-01 (see ~/Games/tools/roadmap.md)

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
- [x] Piece checks: capes/hair skinned to body bones only; weapons have
      no skin and a named attachment point.
      Done v0.5.0: `P_PIECE_BONES` (piece-joint subset of body
      joints), `P_WEAPON_SKIN` / `P_WEAPON_ATTACH` (`ATTACH-*`
      grip node) for `--class weapon`; prop/weapon runs may be
      unrigged.
