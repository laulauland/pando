# VM runtime guide

Pando can attach a persistent BoxLite microVM to a copy-on-write workspace. The
workspace remains editable by host tools and is mounted read-write at
`/workspace`, the guest command's working directory.

VM support is optional. With no runtime configured, `pando create` makes a
host-only workspace. `--no-runtime` always forces that behavior.

## Availability

Pando 0.4 runtime support is qualified only on Linux x86_64 with KVM API version
12. The official Linux x86_64 release and Homebrew bottle include BoxLite.
Release binaries for Apple Silicon retain host-only workspace behavior;
`--runtime boxlite` reports that the release does not include runtime support.

Before changing workspace storage, Pando verifies that `/dev/kvm` is readable
and writable and reports an actionable error when virtualization or permissions
are unavailable.

Source builds enable the runtime explicitly:

```bash
cargo build --release --features microvm-boxlite
```

See [Linux runtime packaging](runtime-packaging.md) for the exact platform
matrix, qualified artifacts, pinned native archive, and provenance evidence.

## Create and use a VM

```bash
pando create feature-x \
  --runtime boxlite \
  --image alpine:3.22 \
  --cpus 2 \
  --memory-mib 512 \
  --allow-unqualified-seccomp

pando exec feature-x -- uname -a
pando shell feature-x
pando stop feature-x
pando remove feature-x
```

`exec` preserves argument boundaries and returns the guest command's exit
status. `shell` opens an interactive `/bin/sh`. Both restart a stopped runtime.
`info` reports the configured image, provider ID, and observed runtime state.

Stopping a VM ends its processes. Its root disk and `/workspace` persist.
Removing the workspace stops and removes its VM before deleting workspace
storage.

## Configuration

Defaults belong in `$PANDO_HOME/config.toml`, normally
`~/.pando/config.toml`:

```toml
[runtime]
runtime = "boxlite"
image = "alpine:3.22"
cpus = 2
memory_mib = 512
allow_unqualified_seccomp = true
```

With `runtime = "boxlite"`, an ordinary `pando create feature-x` creates both
the workspace and VM. Command-line values override configured values. An image
or resource default does not enable a VM by itself; select `runtime = "boxlite"`
or pass `--runtime boxlite`. `--no-runtime` overrides a configured runtime.
Unknown keys and invalid values fail before workspace mutation.

Pando accepts 1–64 CPUs and 128–262144 MiB of memory. When not configured, the
defaults are 2 CPUs and 512 MiB.

`allow_unqualified_seccomp` is Linux-specific and must be omitted on macOS.

## Project environments

Pando provisions `jj` for a `jj`-backed Linux VM, but it does not infer or
install project runtimes. Select an OCI image that already contains the required
language runtime, system packages, and dependency cache.

For example, a Bun project can publish an image containing its pinned Bun
version and build tools:

```dockerfile
FROM oven/bun:1.3.6-alpine
RUN apk add --no-cache bash build-base git
```

Then select an immutable digest:

```toml
[runtime]
runtime = "boxlite"
image = "ghcr.io/example/pando-bun@sha256:..."
cpus = 2
memory_mib = 2048
allow_unqualified_seccomp = true
```

Dependencies must already exist in the workspace or image, or be installable
from an included offline cache. For Bun, use
`bun install --frozen-lockfile --offline` when such a cache is present.

## Network boundary

Networking is always disabled in this release. BoxLite gives the guest loopback
and an unconnected dummy interface, with no default route. Pando intentionally
has no network-enable flag.

This is a runtime constraint as well as a security policy. A standard colocated
`jj` workspace can require three virtiofs mounts: `/workspace`, the canonical
`jj` repository store, and the colocated Git store. With BoxLite 0.9.7 and
libkrun, adding a network device to that topology can exhaust the available
virtio IRQs. Pando therefore fails closed instead of exposing a network mode
that works only for some repository layouts.

## Linux seccomp boundary

Pando fails closed by default on Linux because BoxLite 0.9.7's bundled seccomp
profile terminates the qualified libkrun path with `SIGSYS`.
`--allow-unqualified-seccomp` explicitly acknowledges running with that provider
filter disabled. VM isolation, the BoxLite jailer, sealed mounts, resource
limits, and disabled networking remain active, but this is not equivalent to a
qualified seccomp sandbox. macOS uses Hypervisor.framework and does not expose
this override.

## Shared-filesystem boundary

BoxLite 0.9.7's Linux virtiofs mount preserves atomic exclusive creation,
directories, rename, mmap visibility, and the filesystem primitives used by
`jj`. It does not propagate BSD `flock` or POSIX `fcntl` advisory locks between
host and guest.

Software sharing `/workspace` across the host/guest boundary must use atomic
lock files or directories, or avoid simultaneous access. Pando's live suite
verifies this limitation and concurrent host/guest `jj` integrity.

## `jj` repository mounts

For native `jj` workspaces, Pando mounts the canonical `.jj/repo` store
read-write at the path selected by the workspace's relative `.jj/repo` pointer.
For standard colocated repositories it also mounts the canonical `.git`
directory at the destination selected by the store's `git_target` pointer. The
canonical working-copy files are never mounted.

Pando validates the pointers, destinations, and directory identities before and
after provider creation. Absolute, malformed, mismatched, symlinked,
overlapping, and externally backed layouts fail closed.

On Linux, Pando snapshots the host `jj` executable while creating a `jj`-backed
runtime and installs it at `/usr/local/bin/jj` on the persistent guest root disk.
The temporary staging copy is removed before creation commits. This makes
`pando exec feature-x -- jj status` work with minimal images without exposing
host tool directories.

