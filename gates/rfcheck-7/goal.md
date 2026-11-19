# rfcheck-7: rfcheck's docs are current

Repo: rfcheck (`~/Games/_blender/rfcheck`). One goal, this repo only. Docs and comments only.

## Why

rfcheck-1 (commit 86abd1e) moved the Bevy layer to Bevy 0.20.0: the compat crate is pinned to
`=0.20.0` with the games' features, and `glbkit/compat/README.md`'s "rfcheck Bevy layer"
section records the proof. The other docs still say 0.19, point at `animforge`'s
`dcc/export_check.py` (that tool and file no longer exist), and ask for an owner decision
through "Decisions", a page the owner no longer uses (owner choices are asked in the chat).

## What to do

`grep -nE "0\.19|animforge|Decisions" README.md TODO.md src/*.rs glbkit/README.md glbkit/compat/README.md`
lists every line. For each:

- Where a line says what the games run or what rfcheck checks against today (README.md lines
  6, 50, 124; `src/main.rs` line 6, a comment; `glbkit/README.md` line 30): say Bevy 0.20
  (0.20.0 where the patch level is given). README.md line 124's paragraph should point at
  `glbkit/compat/README.md`'s "rfcheck Bevy layer (2026-11-19, Bevy 0.20.0)" for the proof.
- `animforge` (README.md lines 23 and 107): the comparison is history; say the D_ codes and
  the report's shape were modelled on the game export checker the old animation tool had
  (now retired), without naming the old file.
- Records of the past (TODO.md line 27's heading; `glbkit/compat/README.md` line 33's "Last
  run (2026-10-04, Bevy 0.19.1, ...)"): keep the date and the result; write the version as
  "the Bevy the games ran then (before 0.20)" or similar, without the digits.
- TODO.md line 55, the open "Owner decision (Decisions, 2026-10-06)": keep the open item, say
  it waits on the owner's call in the chat.
- Nothing else: no reformatting, no other rewording, no code. In `src/*.rs` only comment
  lines may change.

## Evidence

`bash gates/rfcheck-7/gate.sh` prints `GATE PASS`. It checks the grep above prints nothing,
that README.md and glbkit/README.md name Bevy 0.20, the scope (docs and comment lines only),
and CI's own checks: `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`,
`cargo test --locked`.

## Out of scope; don't

- Don't edit `gates/`, `examples/`, `bench/`, `glbkit/compat/bevy/`, Cargo files or any code.
- Don't name any other game, company or franchise.
- Don't start Blender or build anything on this Mac.
