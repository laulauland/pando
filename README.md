# pando

`pando` creates isolated, copy-on-write workspaces from a source directory. Each workspace is a CoW clone of the source tree, registered as a native `jj` workspace when the source is a `jj` repository, so you can edit, build, and commit in parallel without disturbing the canonical checkout. 

> `pando` is named after the clonal aspen colony: many trunks, one root system.

## Usage

    pando create <name> [--from <revset>]
    pando list
    pando info <name> [--json]
    pando get  <name> [--json]
    pando cd   <name> [--print]
    pando remove <name> [--keep-jj-workspace]
    pando rm     <name> [--keep-jj-workspace]
    pando completions <shell>

The same CLI is also installed as `pd` for shorter invocations (`pd create`, `pd rm`, …). Examples below use `pando`.

`pando create` runs from the current directory and prints the new workspace's absolute path to stdout, so the common shell idiom is:

    cd "$(pando create feature-x)"

Names cannot contain whitespace or path separators. `--from` takes a `jj` revset that must resolve to exactly one commit; it is silently ignored when the source is not a `jj` repository. With no `--from`, the new workspace is based on the canonical workspace's `@-`.

`pando list` prints an aligned table — `NAME`, `AGE`, `BASE` (jj change id revision), and `JJ` (the registered workspace name, or `-` for non-jj sources):

    NAME       AGE  BASE  JJ
    feature-x  4m   y     pando-feature-x
    plain      1h   -     -

`pando info <name>` prints workspace facts as an aligned table. Pass `--json` for stable JSON output for scripts, including state and workspace paths, canonical root, creation time, and `jj` metadata when present. `pando get <name>` is an alias for `info`.

`pando cd <name>` opens your shell in that workspace. Pass `--print` to print the workspace path instead.

`pando remove` deletes the workspace's state directory and forgets the corresponding `jj` workspace from the canonical repo. `--keep-jj-workspace` skips the `jj` forget step but still deletes the state directory.

`pando completions <shell>` prints a clap-generated completion script to stdout, e.g. `pando completions fish > ~/.config/fish/completions/pando.fish`.

## How it works

State lives under `$PANDO_HOME` (default `~/.local/state/pando`), one directory per workspace, each containing a `meta.toml` with the canonical root, the workspace path, and any `jj` registration data. A `.lock` file in `$PANDO_HOME` serializes lifecycle operations so concurrent `pando` invocations do not race.

The copy-on-write backend is selected at compile time. On macOS, files are cloned with APFS `clonefile(2)`. On Linux, the workspace is an OverlayFS mount with the source as `lowerdir` and pando-owned `upperdir`/`workdir`, exposed at `merged`. On other platforms, the source tree is copied recursively.

When the source contains a `.jj/` directory, pando uses `jj-lib` directly (no shelling out to `jj`) to register the new workspace as `pando-<name>` against the canonical repo, point its `@` at the resolved base commit, and reconcile the working copy. Author identity for the pando-created working-copy commit is read from your `jj` user config (`~/.config/jj/config.toml` or `$XDG_CONFIG_HOME/jj/config.toml`). If `jj` registration fails, the state directory is rolled back so `create` is atomic from the user's point of view.

`remove` is the inverse: it forgets the `jj` workspace via a transaction on the canonical repo and then removes the state directory. If the `jj` forget step fails, the state directory is left in place so the operation can be retried.

## Install

With Homebrew:

    brew install laulauland/tap/pando

Or with the release installer:

    curl -fsSL https://raw.githubusercontent.com/laulauland/pando/main/scripts/install.sh | bash

Set `BIN_DIR` to choose the install directory, or `PANDO_VERSION` to install a specific release:

    curl -fsSL https://raw.githubusercontent.com/laulauland/pando/main/scripts/install.sh | BIN_DIR=~/.local/bin PANDO_VERSION=0.2.0 bash

The installer also writes bash, zsh, and fish completions for both `pando` and `pd` into user completion directories by default. Set `INSTALL_COMPLETIONS=0` to skip them, or override `BASH_COMPLETION_DIR`, `ZSH_COMPLETION_DIR`, or `FISH_COMPLETION_DIR`.

## Build

    cargo build --release

The binaries land at `target/release/pando` and `target/release/pd`. Tests: `cargo test`. Integration tests under `tests/jj_registration.rs` are skipped when the `jj` binary is not on `PATH`.

## Benchmark

Measure real workspace creation against a source directory:

    cargo bench --bench create -- /path/to/workspace --samples 10 --output target/bench-results/current.json

Compare multiple candidate Pando checkouts against the same workload:

    cargo bench --bench compare -- /path/to/workspace \
      --candidate /path/to/pando-baseline \
      --candidate /path/to/pando-candidate \
      --samples 10 \
      --output target/bench-results/compare-run

The create benchmark times only `create_workspace`; cleanup runs after each measured sample and the JSON result records median, minimum, maximum, and cleanup status.
