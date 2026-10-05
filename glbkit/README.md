# glbkit

GLB building blocks shared by the factory's Rust GLB tools: rfcheck,
weightforge, and wrapforge. Zero dependencies, MIT.

- `container`: parse/write the glTF 2.0 binary container (bounds-checked,
  spec padding: what Blender and Bevy's loader expect).
- `accessor`: resolve an accessor against the BIN chunk once, then read
  and write elements without panics (normalized ints per spec).
- `rig`: the rig contract (`DEF-` bones, 4 influences, weight tolerance,
  piece-mesh words), so the checker and the fixer cannot drift.

No JSON type: callers parse the JSON chunk with what they already use and
pass accessor fields in as plain values.

retopoforge keeps its own reader: it is a byte-exact cgltf port pinned by
a differential oracle.

## Use

```toml
glbkit = { git = "https://github.com/chris-straka/rfcheck.git", rev = "<sha>" }
```

Pin a `rev`, same as other cross-repo git deps here (`af-bevy` in HLL).

## Compatibility gates

`compat/` holds the checks that GLBs written through glbkit load in the
tools the games use: a headless Bevy 0.19 loader (`compat/bevy`) and a
headless Blender import (`compat/blender_import.py`). See
`compat/README.md`.
