#!/usr/bin/env bash
# Gate for rfcheck-1 (game-tools docs/plans/rfcheck.md): rfcheck's Bevy layer (B_*, L_*) moved
# to the games' Bevy 0.20.0 and proven against its glTF loader.
#  - glbkit/compat/bevy pins bevy =0.20.0 with the games' features (HLH's bevy/Cargo.toml:
#    default-features off; std, 2d, 3d, ui, ktx2, zstd_rust; no jpeg);
#  - `cargo run --release --example bevy_fixtures` writes every fixture in the table below,
#    including the cases a reading of bevy_gltf 0.20 adds (required extensions it now parses,
#    image types it decodes);
#  - on each fixture rfcheck raises exactly the B_*/L_* code the table names (none for ok_*);
#  - Bevy 0.20's loader (glbkit/compat/bevy, example `check`) agrees: ok_* and the
#    convention-only L_LOD_PAIR load; the B_* "fail" fixtures fail to load; the B_* "lost"
#    fixtures load but lose what the code says (fewer animated targets than channels, fewer
#    named clips than clips).
# Builds Bevy (release; give it 8 cores). Prints PASS/FAIL per check, ends with GATE PASS or
# GATE FAIL.
set -u
cd "$(git rev-parse --show-toplevel)" || exit 2
FAILS=0
pass() { echo "PASS  $1"; }
fail() { echo "FAIL  $1"; FAILS=$((FAILS + 1)); }
show() { sed 's/^/      /' | head -n "${2:-20}"; }
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
command -v cargo >/dev/null 2>&1 || { fail "no cargo on this machine"; echo "GATE FAIL ($FAILS)"; exit 1; }

# fixture  rfcheck-code  loader (ok | fail | lost-targets | lost-names)
cat > "$T/table" <<'TAB'
ok_base                  -               ok
ok_lod_pair              -               ok
ok_ext_used_specular     -               ok
ok_image_hdr             -               ok
ok_image_mime_upper      -               ok
B_EXT_REQUIRED           B_EXT_REQUIRED  fail
B_EXT_REQUIRED_specular  B_EXT_REQUIRED  fail
B_EXT_REQUIRED_volume    B_EXT_REQUIRED  fail
B_PRIM_MODE              B_PRIM_MODE     fail
B_NODE_CYCLE             B_NODE_CYCLE    fail
B_IMAGE_CODEC_webp       B_IMAGE_CODEC   fail
B_IMAGE_CODEC_mismatch   B_IMAGE_CODEC   fail
B_IMAGE_CODEC_jpeg       B_IMAGE_CODEC   fail
B_IMAGE_CODEC_jpg_alias  B_IMAGE_CODEC   fail
B_MORPH_LIMIT            B_MORPH_LIMIT   fail
B_ANIM_ORPHAN            B_ANIM_ORPHAN   lost-targets
B_ANIM_PATH              B_ANIM_PATH     lost-targets
B_CLIP_NAME              B_CLIP_NAME     lost-names
L_LOD_PAIR               L_LOD_PAIR      ok
TAB

echo "== the pin"
python3 - glbkit/compat/bevy/Cargo.toml > "$T/pin.out" <<'PY'
import re, sys
s = open(sys.argv[1]).read()
m = re.search(r'^bevy\s*=\s*\{([^}]*)\}', s, re.M)
if not m:
    print("no bevy = { ... } line"); sys.exit(1)
body = m.group(1)
ver = re.search(r'version\s*=\s*"([^"]+)"', body)
dfl = re.search(r'default-features\s*=\s*(true|false)', body)
feats = set(re.findall(r'"([^"]+)"', (re.search(r'features\s*=\s*\[([^\]]*)\]', body) or [None, ""])[1] or ""))
want = {"std", "2d", "3d", "ui", "ktx2", "zstd_rust"}
bad = []
if not ver or ver.group(1) != "=0.20.0": bad.append(f"version {ver.group(1) if ver else None}, want =0.20.0")
if not dfl or dfl.group(1) != "false": bad.append("default-features is not false")
if feats != want: bad.append(f"features {sorted(feats)}, want {sorted(want)}")
print("; ".join(bad) or "bevy =0.20.0, default-features false, " + ", ".join(sorted(feats)))
sys.exit(1 if bad else 0)
PY
if [ $? -eq 0 ]; then pass "compat pin: $(cat "$T/pin.out")"; else fail "compat pin: $(cat "$T/pin.out")"; fi

echo "== fixtures"
if ! cargo build --release --locked > "$T/b1.out" 2>&1 || ! cargo run --release --locked --example bevy_fixtures -- "$T/fx" > "$T/fx.out" 2>&1; then
  fail "rfcheck or bevy_fixtures did not build/run"; tail -20 "$T/b1.out" "$T/fx.out" | show 30
  echo "GATE FAIL ($FAILS)"; exit 1
fi
RF="${CARGO_TARGET_DIR:-$PWD/target}/release/rfcheck"
while read -r name code loader; do
  [ -f "$T/fx/$name.glb" ] || fail "fixture $name.glb not written"
done < "$T/table"
extra=$(for f in "$T"/fx/*.glb; do n=$(basename "$f" .glb); awk -v n="$n" '$1 == n {f=1} END {exit !f}' "$T/table" || echo "$n"; done)
[ -z "$extra" ] || fail "fixtures the gate has no expectation for: $(echo $extra)"

echo "== rfcheck on each fixture"
while read -r name code loader; do
  [ -f "$T/fx/$name.glb" ] || continue
  got=$("$RF" --json "$T/fx/$name.glb" 2>/dev/null | python3 -c '
import json, sys
r = json.load(sys.stdin)
items = r if isinstance(r, list) else r.get("files", [r])
print(" ".join(sorted({d["code"] for it in items for d in it.get("diags", it.get("findings", [])) if d["code"][:2] in ("B_", "L_")})) or "-")')
  if [ "$got" = "$code" ]; then pass "rfcheck $name: $got"; else fail "rfcheck $name: $got, want $code"; fi
done < "$T/table"

echo "== Bevy 0.20's loader on each fixture"
( cd glbkit/compat/bevy && cargo run --release --example check -- "$T"/fx/*.glb ) > "$T/load.out" 2> "$T/load.err"
if ! grep -qE '^(ok  |FAIL) ' "$T/load.out"; then
  fail "the compat loader did not run"; tail -30 "$T/load.err" | grep -E "error|warning: unused" | show 20
else
  while read -r name code loader; do
    line=$(grep -E "^(ok  |FAIL) [^ ]*/$name\.glb:" "$T/load.out")
    verdict=$(python3 - "$loader" "$line" <<'PY'
import re, sys
want, line = sys.argv[1], sys.argv[2]
if not line:
    print("no line"); sys.exit(1)
ok = line.startswith("ok")
m = re.search(r"(\d+) clips \((\d+) named, (\d+) animated targets\)", line)
clips, named, targets = (int(x) for x in m.groups()) if m else (None, None, None)
if want == "ok": good = ok
elif want == "fail": good = not ok
elif want == "lost-targets": good = ok and targets is not None and targets < 2
elif want == "lost-names": good = ok and named is not None and clips is not None and named < clips
print(line.split(": ", 1)[-1][:150]); sys.exit(0 if good else 1)
PY
)
    if [ $? -eq 0 ]; then pass "loader $name ($loader): $verdict"; else fail "loader $name, want $loader: $verdict"; fi
  done < "$T/table"
fi
grep -q "Bevy 0.20" "$T/load.out" && pass "the loader says Bevy 0.20" || fail "the loader's summary doesn't say Bevy 0.20: $(tail -1 "$T/load.out")"

if [ "$FAILS" -eq 0 ]; then echo "GATE PASS"; exit 0; else echo "GATE FAIL ($FAILS)"; exit 1; fi
