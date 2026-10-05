# glbkit compatibility gates

Proof that GLBs written through glbkit (weightforge's fix/fixture output)
load in the tools the games use, and that Blender's own GLBs read back.

## Bevy (headless, no GPU)

```sh
cd glbkit/compat/bevy
cargo run --release -- a.glb b.glb ...
```

Loads each file through Bevy 0.19.1's glTF loader (same pin as hll-bevy),
spawns the default scene, and checks that every declared skin becomes
`SkinnedMesh` entities with joint indices and weights. Exit 1 on any
failure. Standalone crate (own `[workspace]`): Bevy is a long first build.

The `jpeg` feature is on because AI-generated GLBs (Tripo, UniRig output)
embed JPEG textures. Games that load those files need it too: without it
Bevy fails with `invalid image mime type: image/jpeg`.

## Blender (headless)

```sh
/Applications/Blender.app/Contents/MacOS/Blender --background --factory-startup \
  --python glbkit/compat/blender_import.py -- [--export-dir DIR] a.glb ...
```

Imports each file, checks vertex groups (weighted verts sum to 1 within
1e-3, max influences), and with `--export-dir` re-exports through
Blender's writer for the reverse direction. Exit 1 on any failure.

## Last run (2026-10-04, Bevy 0.19.1, Blender 5.2.1)

| Set | Bevy | Blender |
|---|---|---|
| weightforge fixtures (7) + `fix` outputs on fixtures and 12 owner-corpus characters (15 distinct) | 22/22 | 15/15 |
| Blender exports of those → `weights fix` → back | 15/15 | 15/15 |
| Truncated GLB (negative control) | rejected | rejected |

Round-tripped files carry the same rfcheck codes as their sources.
