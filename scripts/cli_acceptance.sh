#!/usr/bin/env bash
# Manual CLI acceptance smoke test.
# Uses the real platform backend selected by the binary. On Linux this may require
# overlayfs mount privileges; on macOS it exercises APFS clonefile.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/pando-cli-acceptance.XXXXXX")"
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

alpha="$(cd "$SOURCE" && $PANDO create alpha --from ignored-outside-jj)"
beta="$(cd "$SOURCE" && $PANDO create beta --from ignored-outside-jj)"

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

list_output="$($PANDO list)"
grep -q $'NAME\tAGE\tBASE\tJJ' <<< "$list_output"
grep -q $'alpha\t.*\t-\t-' <<< "$list_output"
grep -q $'beta\t.*\t-\t-' <<< "$list_output"
"$PANDO" remove alpha --keep-jj-workspace
assert_missing "$PANDO_HOME/alpha"
assert_exists "$PANDO_HOME/beta"
"$PANDO" rm beta
assert_missing "$PANDO_HOME/beta"

echo "CLI acceptance passed."
