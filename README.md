# pando

`pando` creates isolated, copy-on-write workspaces from a source directory. Each workspace is a CoW clone of the source tree, registered as a native `jj` workspace when the source is a `jj` repository, so you can edit, build, and commit in parallel without disturbing the canonical checkout. 

> `pando` is named after the clonal aspen colony: many trunks, one root system.

## Usage

    pando create <name> [--from <revset>] [--runtime boxlite] [--image <image>] [--cpus <count>] [--memory-mib <MiB>]
    pando exec <name> -- <command> [args...]
    pando shell <name>
    pando stop <name>
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

Build with `--features microvm-boxlite` to attach an optional BoxLite micro-VM to a workspace. The smallest runtime workflow is:

    pando create feature-x --runtime boxlite --image alpine:3.22 --cpus 2 --memory-mib 512
    pando exec feature-x -- uname -a
    pando shell feature-x
    pando stop feature-x
    pando remove feature-x

`exec` preserves argument boundaries and returns the guest command's exit status. `shell` opens an interactive `/bin/sh`; both commands restart a stopped runtime. Runtime workspaces are mounted read-write at `/workspace`, which is also the guest command's working directory. `info` reports the configured image, provider ID, and observed runtime state. Removing a runtime workspace stops and removes its VM before deleting the copy-on-write workspace.

Runtime creation validates CPU (1–64) and memory (128–262144 MiB) limits before workspace mutation; defaults are 2 CPUs and 512 MiB. Networking is always disabled in this release: BoxLite supplies only loopback and an unconnected dummy interface, with no default route. There is intentionally no network-enable flag yet. Guest root-disk state and `/workspace` persist across `stop`, while guest processes do not.

On Linux, Pando fails closed by default because BoxLite 0.9.7's bundled seccomp profile terminates the qualified libkrun path with `SIGSYS`. `--allow-unqualified-seccomp` explicitly acknowledges running with that provider filter disabled; VM isolation, the BoxLite jailer, sealed mounts, resource limits, and disabled networking remain active, but this is not equivalent to a qualified seccomp sandbox. macOS uses Hypervisor.framework and does not expose this Linux-only override.

BoxLite 0.9.7's Linux virtiofs mount preserves atomic exclusive creation, directories, rename, mmap visibility, and the filesystem primitives used by `jj`. It does **not** propagate BSD `flock` or POSIX `fcntl` advisory locks between host and guest: software sharing `/workspace` across that boundary must use atomic lock files/directories or avoid simultaneous access. Pando's live suite deliberately verifies both this limitation and concurrent host/guest `jj` integrity.

For native `jj` workspaces, Pando also mounts only the canonical `.jj/repo` store read-write at the guest path selected by the workspace's unchanged relative `.jj/repo` pointer. The canonical working copy is not mounted. Pando validates that pointer against the recorded canonical store before creating the VM; absolute, malformed, mismatched, overlapping, and externally backed store layouts fail closed. In particular, this stage supports self-contained/non-colocated jj repositories; a colocated Git backend depends on the canonical `.git` outside `.jj/repo` and is rejected rather than exposing it.

Names cannot contain whitespace or path separators. `--from` takes a `jj` revset that must resolve to exactly one commit; it is silently ignored when the source is not a `jj` repository. With no `--from`, the new workspace is based on the canonical workspace's `@-`.

`pando list` prints an aligned table — `NAME`, `AGE`, `BASE` (jj change id revision), and `JJ` (the registered workspace name, or `-` for non-jj sources):

    NAME       AGE  BASE  JJ
    feature-x  4m   y     pando-feature-x
    plain      1h   -     -

`pando info <name>` prints workspace facts as an aligned table. Pass `--json` for stable JSON output for scripts, including state and workspace paths, canonical root, creation time, and `jj` metadata when present. `pando get <name>` is an alias for `info`.

`pando cd <name>` opens your shell in that workspace. Pass `--print` to print the workspace path instead.

`pando remove` deletes the workspace and its state, then forgets the corresponding `jj` workspace from the canonical repo. `--keep-jj-workspace` skips the `jj` forget step but still deletes the Pando-owned files.

`pando completions <shell>` prints a clap-generated completion script to stdout, e.g. `pando completions fish > ~/.config/fish/completions/pando.fish`.

## How it works

Pando separates user-facing workspaces from implementation state under `$PANDO_HOME` (default `~/.pando`):

    ~/.pando/
    ├── workspaces/
    │   └── <name>/
    └── state/
        ├── .lock
        └── <name>/
            ├── meta.toml
            └── overlay/       # Linux only
                ├── upper/
                └── work/

The path under `workspaces/<name>` is the directory to edit or mount into a VM or container. `meta.toml` records the canonical root, workspace path, creation time, and any `jj` registration data. The state lock serializes lifecycle operations so concurrent `pando` invocations do not race.

On the first operational command after upgrading to 0.3, Pando automatically migrates workspaces from the previous default `~/.local/state/pando` layout. Linux OverlayFS workspaces are unmounted, moved, and remounted at their new paths; workspace changes and `jj` registrations are preserved. Custom `$PANDO_HOME` directories are migrated in place.

The copy-on-write backend is selected at compile time. On macOS, files are cloned with APFS `clonefile(2)`. On Linux, `workspaces/<name>` is an OverlayFS mount with the source as `lowerdir` and Pando-owned `upperdir`/`workdir` under `state/<name>/overlay`. On other platforms, the source tree is copied recursively.

When the source contains a `.jj/` directory, pando uses `jj-lib` directly (no shelling out to `jj`) to register the new workspace as `pando-<name>` against the canonical repo, point its `@` at the resolved base commit, and reconcile the working copy. Author identity for the pando-created working-copy commit is read from your `jj` user config (`~/.config/jj/config.toml` or `$XDG_CONFIG_HOME/jj/config.toml`). If `jj` registration fails, the state directory is rolled back so `create` is atomic from the user's point of view.

`remove` is the inverse: it forgets the `jj` workspace via a transaction on the canonical repo and then removes the workspace and state directories. If the `jj` forget step fails, both are left in place so the operation can be retried.

## Install

With Homebrew:

    brew install laulauland/tap/pando

Or with the release installer:

    curl -fsSL https://raw.githubusercontent.com/laulauland/pando/main/scripts/install.sh | bash

Set `BIN_DIR` to choose the install directory, or `PANDO_VERSION` to install a specific release:

    curl -fsSL https://raw.githubusercontent.com/laulauland/pando/main/scripts/install.sh | BIN_DIR=~/.local/bin PANDO_VERSION=0.3.1 bash

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
