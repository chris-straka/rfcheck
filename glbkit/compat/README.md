# glbkit compatibility gates

Proof that GLBs written through glbkit (weightforge's fix/fixture output)
load in the tools the games use, and that Blender's own GLBs read back.

## Bevy (headless, no GPU)

```sh
cd glbkit/compat/bevy
cargo run --release --example check -- a.glb b.glb ...
```

Loads each file through Bevy 0.20.0's glTF loader, built with the games'
features (see below), spawns the default scene, and checks that every
declared skin becomes `SkinnedMesh` entities with joint indices and weights.
Exit 1 on any failure. Standalone crate (own `[workspace]`): Bevy is a long
first build, and it needs the Rust Bevy 0.20 asks for (1.97 or newer).

`jpeg` is off, as in the games: a GLB with a JPEG texture (Tripo and UniRig
output embed them) fails to load in a game, and fails here too.

## Blender (headless)

```sh
/Applications/Blender.app/Contents/MacOS/Blender --background --factory-startup \
  --python glbkit/compat/blender_import.py -- [--export-dir DIR] a.glb ...
```

Imports each file, checks vertex groups (weighted verts sum to 1 within
1e-3, max influences), and with `--export-dir` re-exports through
Blender's writer for the reverse direction. Exit 1 on any failure.

## Last run (2026-10-04, the Bevy the games ran then (before 0.20), Blender 5.2.1)

| Set | Bevy | Blender |
|---|---|---|
| weightforge fixtures (7) + `fix` outputs on fixtures and 12 owner-corpus characters (15 distinct) | 22/22 | 15/15 |
| Blender exports of those → `weights fix` → back | 15/15 | 15/15 |
| Truncated GLB (negative control) | rejected | rejected |

Round-tripped files carry the same rfcheck codes as their sources.

## rfcheck Bevy layer (2026-11-19, Bevy 0.20.0)

The crate is built the way the games build Bevy (HLH's `bevy/Cargo.toml`: no default
features; `std`, `2d`, `3d`, `ui`, `ktx2`, `zstd_rust`), so a file loads here exactly when it
loads in a game. `jpeg` is off, as in the games.

`cargo run --release --example bevy_fixtures -- DIR` in the rfcheck root writes one GLB per
case; `bash gates/rfcheck-1/gate.sh` runs rfcheck and this loader on each and holds them to the
table below (GATE PASS on Linux, fleet tsa2uc3n).

| Fixture | rfcheck | Bevy 0.20 loader |
|---|---|---|
| `ok_base`, `ok_lod_pair` | clean | loads |
| `ok_ext_used_specular` (KHR_materials_specular used, not required) | clean | loads |
| `ok_image_hdr` (`image/vnd.radiance`) | clean | loads (the games build `hdr`) |
| `ok_image_mime_upper` (`image/PNG`) | clean | loads (Bevy lower-cases the type) |
| `B_EXT_REQUIRED` (Draco), `_specular`, `_volume` | fail | "invalid glTF: extensionsRequired" |
| `B_PRIM_MODE` (fan) | fail | "unsupported primitive mode" |
| `B_NODE_CYCLE` | fail | "must be a tree" |
| `B_IMAGE_CODEC_webp`, `_mismatch` (PNG bytes as KTX2), `_jpeg`, `_jpg_alias` | fail | fail to decode |
| `B_MORPH_LIMIT` (257 targets) | fail | "Bevy only supports up to 256 morph targets" |
| `B_ANIM_ORPHAN`, `B_ANIM_PATH` (2 channels) | fail | loads, 1 animated target |
| `B_CLIP_NAME` (2 x "idle") | fail | loads, 2 clips / 1 named |
| `L_LOD_PAIR` | fail | loads (game convention, not a loader rule) |

What moving to 0.20 changed in rfcheck (`src/bevy.rs`):

- `hdr` is among the default codecs: the games' Bevy decodes it (`common_api`), and rfcheck
  raised `B_IMAGE_CODEC` on an HDR texture that loads (`ok_image_hdr`).
- Image types are read as Bevy's `ImageFormat::from_mime_type` reads them: lower-cased, by
  exact name. `image/PNG` is fine (it was a false `B_IMAGE_CODEC`); `image/jpg`, `image/x-dds`
  and `image/x-hdr` are not names Bevy knows (rfcheck took them as JPEG, DDS and HDR).
- A data URI is decoded by the image's own `mimeType` when it has one, as Bevy does.
- Required extensions: 0.20 also builds gltf with `KHR_materials_specular`; like `_volume`
  it is read when used but still refused when required (gltf-json's `ENABLED_EXTENSIONS` is
  unchanged), so the accepted list stays the six it was.

On HLH at b5053111 the 276 tracked GLBs have no `B_*` or `L_*` finding before or after
(fleet jrzyqhsg).

