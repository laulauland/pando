#!/usr/bin/env bash
# Manual V1 CLI acceptance smoke test.
# Uses the real platform backend selected by the binary. On Linux this may require
# overlayfs mount privileges; on macOS it exercises APFS clonefile.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/pando-v1-acceptance.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

assert_exists() {
  [[ -e "$1" ]] || { echo "missing expected path: $1" >&2; exit 1; }
}

assert_missing() {
  [[ ! -e "$1" ]] || { echo "unexpected remaining path: $1" >&2; exit 1; }
}

export PANDO_HOME="$TMP/home"
SOURCE="$TMP/source"
mkdir -p "$SOURCE/nested"
printf 'canonical\n' > "$SOURCE/README.md"
printf 'nested canonical\n' > "$SOURCE/nested/file.txt"

cargo build --quiet --manifest-path "$ROOT/Cargo.toml"
PANDO="$ROOT/target/debug/pando"

alpha="$($PANDO create alpha --from "$SOURCE")"
beta="$($PANDO create beta --from "$SOURCE")"

[[ -d "$alpha" ]] || { echo "alpha path was not created: $alpha" >&2; exit 1; }
[[ -d "$beta" ]] || { echo "beta path was not created: $beta" >&2; exit 1; }
[[ "$alpha" != "$beta" ]] || { echo "two workspace paths unexpectedly match" >&2; exit 1; }

grep -q 'canonical' "$alpha/README.md"
grep -q 'canonical' "$beta/README.md"
printf 'alpha edit\n' > "$alpha/README.md"
printf 'beta edit\n' > "$beta/README.md"
grep -q 'canonical' "$SOURCE/README.md"
grep -q 'alpha' "$alpha/README.md"
grep -q 'beta' "$beta/README.md"

"$PANDO" list | grep -q $'alpha\t'
"$PANDO" list | grep -q $'beta\t'
"$PANDO" destroy alpha --keep-jj-workspace
assert_missing "$PANDO_HOME/alpha"
assert_exists "$PANDO_HOME/beta"
"$PANDO" destroy beta
assert_missing "$PANDO_HOME/beta"

echo "V1 CLI acceptance passed."
