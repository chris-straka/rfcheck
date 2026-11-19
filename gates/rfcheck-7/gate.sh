#!/usr/bin/env bash
# Gate for rfcheck-7 (game-tools docs/plans/rfcheck.md): rfcheck's docs are current.
#  - `grep -nE "0\.19|animforge|Decisions" README.md TODO.md src/*.rs glbkit/README.md
#    glbkit/compat/README.md` prints nothing;
#  - README.md and glbkit/README.md name Bevy 0.20;
#  - scope: the worker changed only README.md, TODO.md, glbkit/README.md,
#    glbkit/compat/README.md and comment lines in src/*.rs (no code);
#  - CI's checks: cargo fmt --check, cargo clippy --all-targets --locked -- -D warnings,
#    cargo test --locked.
# Builds rfcheck (small). Prints PASS/FAIL per check, ends with GATE PASS or GATE FAIL.
set -u
cd "$(git rev-parse --show-toplevel)" || exit 2
SLUG=rfcheck-7
FAILS=0
pass() { echo "PASS  $1"; }
fail() { echo "FAIL  $1"; FAILS=$((FAILS + 1)); }
show() { sed 's/^/      /' | head -n "${2:-20}"; }
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
BASE=$(git log -1 --format=%H -- gates/$SLUG)
[ -n "$BASE" ] || { fail "cannot find the commit that added the gate"; BASE=HEAD; }

echo "== stale words"
hits=$(grep -nE "0\.19|animforge|Decisions" README.md TODO.md src/*.rs glbkit/README.md glbkit/compat/README.md)
if [ -z "$hits" ]; then pass "no 0.19, animforge or Decisions left"; else fail "stale lines left:"; echo "$hits" | show; fi
for f in README.md glbkit/README.md; do grep -q "0\.20" "$f" && pass "$f names Bevy 0.20" || fail "$f doesn't name Bevy 0.20"; done

echo "== scope"
mine=$(for c in $(git log --format=%H --grep "^carbon $SLUG: " "$BASE..HEAD"); do git diff-tree --no-commit-id --name-only -r "$c"; done)
touched=$( (echo "$mine"; git status --porcelain | cut -c4-) | grep -v '^$' | sort -u)
other=$(echo "$touched" | grep -v -E '^(README\.md|TODO\.md|glbkit/README\.md|glbkit/compat/README\.md|src/[^/]*\.rs)$')
if [ -z "$other" ]; then pass "only the docs and src/*.rs changed"; else fail "other files changed:"; echo "$other" | show; fi
code=$(git diff "$BASE" -- src/*.rs | grep -E '^[+-]' | grep -vE '^(\+\+\+|---)' | grep -vE '^[+-]\s*(//.*)?$')
if [ -z "$code" ]; then pass "src/*.rs: comment lines only"; else fail "src/*.rs: code lines changed:"; echo "$code" | show 10; fi

echo "== CI's checks"
command -v cargo >/dev/null 2>&1 || { fail "no cargo"; echo "GATE FAIL ($FAILS)"; exit 1; }
if cargo fmt --check > "$T/fmt" 2>&1; then pass "cargo fmt --check"; else fail "cargo fmt --check"; show 8 < "$T/fmt"; fi
if cargo clippy --all-targets --locked -- -D warnings > "$T/clippy" 2>&1; then pass "cargo clippy --all-targets --locked -- -D warnings"; else fail "cargo clippy"; grep -E '^(error|warning)' -A3 "$T/clippy" | show 12; fi
if cargo test --locked > "$T/test" 2>&1; then pass "cargo test --locked: $(grep -E '^test result: ok' "$T/test" | awk '{s+=$4} END {print s+0}') passed"; else fail "cargo test --locked"; grep -E 'FAILED|panicked' "$T/test" | show 10; fi

if [ "$FAILS" -eq 0 ]; then echo "GATE PASS"; exit 0; else echo "GATE FAIL ($FAILS)"; exit 1; fi
