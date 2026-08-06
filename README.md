# Pando

Run features, experiments, and coding agents in parallel without disturbing your
main checkout.

Pando creates a writable workspace from the directory you are in. On Linux and
macOS it uses copy-on-write storage, and in a `jj` repository each workspace is
registered as a native `jj` workspace. On supported Linux systems, a workspace
can also have its own persistent microVM for builds, tests, and agent commands.

> Pando is named after the clonal aspen colony: many trunks, one root system.

## What you can do

- Give each feature or coding agent its own working copy.
- Start a `jj` workspace from Pando's default base or another revision.
- Keep using host editors and agents while commands run inside a microVM.
- Fetch dependencies and run networked tools inside the microVM.
- Stop a VM and resume it later without losing its root filesystem or workspace.
- Remove an experiment without touching the canonical checkout.

## Install

With Homebrew:

```bash
brew install laulauland/tap/pando
```

Or with the release installer:

```bash
curl -fsSL https://raw.githubusercontent.com/laulauland/pando/main/scripts/install.sh | bash
```

The installer accepts `BIN_DIR` and `PANDO_VERSION` overrides and installs shell
completions by default. See `pando --help` for the complete command reference.
The same CLI is also installed as `pd` for shorter commands.

## Create a parallel workspace

From any source directory:

```bash
cd my-project
cd "$(pando create feature-x)"
```

Edit, build, and commit normally. In a `jj` repository, Pando registers a native
workspace automatically. By default it starts from the canonical workspace's
`@-`; choose another commit with a revset:

```bash
pando create investigate-bug --from 'main@origin'
```

Inspect your workspaces and return to one later:

```bash
pando list
pando info feature-x
pando cd feature-x
```

When the work is finished:

```bash
pando remove feature-x
```

Removing a Pando workspace also forgets its registered `jj` workspace. Use
`--keep-jj-workspace` only when you deliberately want to keep that registration.

## Add a persistent VM

Official Linux x86_64 release binaries include optional BoxLite microVM support.
Create a VM-backed workspace by selecting an OCI image:

```bash
pando create feature-x \
  --runtime boxlite \
  --image alpine:3.22 \
  --cpus 2 \
  --memory-mib 512 \
  --allow-unqualified-seccomp

pando exec feature-x -- uname -a
pando shell feature-x
```

Your host tools edit the Pando workspace while VM commands see the same files at
`/workspace`:

```bash
workspace="$(pando cd feature-x --print)"
# Point your host editor or agent at $workspace.
pando exec feature-x -- jj status
```

Choose an image containing your project's toolchain to run its builds and tests
the same way.

`pando stop feature-x` ends guest processes but preserves the guest root disk and
workspace. The next `exec` or `shell` starts it again. `pando remove feature-x`
stops and removes the VM as well as the workspace.

Repeated VM options can live in `~/.pando/config.toml`:

```toml
[runtime]
runtime = "boxlite"
image = "alpine:3.22"
cpus = 2
memory_mib = 512
allow_unqualified_seccomp = true # required acknowledgement on Linux
```

With this configuration, `pando create feature-x` creates the workspace and its
VM. Use `--no-runtime` for a host-only exception.

### Runtime support and limitations

VM-backed workspaces currently require Linux x86_64, KVM API version 12, and
read/write access to `/dev/kvm`. Release binaries on macOS provide host-only
workspaces.

VMs have outbound networking enabled, including DNS and HTTPS, so project tools
can fetch dependencies into the persistent workspace or guest root disk.

On Linux, BoxLite 0.9.7 also requires an explicit acknowledgement that its
provider seccomp filter is disabled. VM isolation, the BoxLite jailer, sealed
mounts, and resource limits remain active, but this is not equivalent to a
qualified seccomp sandbox.

Host and guest processes sharing `/workspace` must not rely on BSD `flock` or
POSIX `fcntl` advisory locks being coordinated across that boundary. See the
[runtime guide](docs/runtime.md) before enabling VM-backed workspaces. The
[runtime packaging record](docs/runtime-packaging.md) contains the full platform
matrix, qualification evidence, provenance, and notices.

## How Pando fits together

```text
canonical source
    └── copy-on-write workspace      edit with host tools
            ├── native jj workspace  share repository history
            └── optional microVM      run commands in /workspace
```

Pando owns workspace and runtime lifecycle. It does not choose an agent, infer
project dependencies, or build an OCI image. This narrow boundary lets editors,
coding agents, and scripts use the same commands:

```bash
pando create <name>
pando exec <name> -- <command> [args...]
pando shell <name>
pando stop <name>
pando list
pando info <name> [--json]
pando cd <name> [--print]
pando remove <name>
```

For storage, `jj`, and lifecycle internals, see
[Architecture](docs/architecture.md). To build or benchmark Pando, see
[Contributing](CONTRIBUTING.md).
