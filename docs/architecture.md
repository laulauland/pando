# Architecture

Pando separates user-facing workspaces from implementation state under
`$PANDO_HOME`, which defaults to `~/.pando`:

```text
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
```

The path under `workspaces/<name>` is the directory users edit and Pando mounts
into a VM. `meta.toml` records the canonical root, workspace path, creation time,
and `jj` registration data. A state lock serializes lifecycle operations so
concurrent Pando processes do not race.

## Copy-on-write storage

The storage backend is selected at compile time:

- macOS clones files with APFS `clonefile(2)`.
- Linux mounts OverlayFS with the source as `lowerdir` and Pando-owned
  `upperdir` and `workdir` directories under `state/<name>/overlay`.
- Other platforms copy the source tree recursively.

On the first operational command, Pando automatically migrates workspaces found
in the previous `~/.local/state/pando` layout. Linux OverlayFS workspaces are
unmounted, moved, and remounted. Workspace changes and `jj` registrations are
preserved. Custom `$PANDO_HOME` directories migrate in place.

## Native `jj` workspaces

When the source contains `.jj/`, Pando uses `jj-lib` directly to register
`pando-<name>` against the canonical repository, point its `@` at the requested
base commit, and reconcile the working copy. It does not shell out to `jj` for
registration.

With no `--from`, creation uses the canonical workspace's `@-`. `--from` must be
a revset that resolves to exactly one commit. It is ignored for non-`jj` source
directories.

Author identity for the Pando-created working-copy commit comes from the user's
`jj` configuration. If registration fails, Pando rolls back its state so
creation is atomic from the user's perspective.

Removal performs the inverse transaction: it forgets the registered workspace,
then removes Pando's workspace and state directories. If the `jj` transaction
fails, both directories remain so the operation can be retried.

VM mount topology and its security checks are documented in the
[VM runtime guide](runtime.md).

The disk-backed BoxLite path does not attach its otherwise empty shared
virtiofs device. `/workspace`, canonical `.jj/repo`, and canonical `.git`
remain independent validated mounts; the saved x86_64 virtio IRQ is used by
the outbound network device without mounting any broader canonical directory.

## Lifecycle metadata and automation

`pando list` provides human-readable workspace status. For automation,
`pando info <name> --json` provides stable JSON including workspace paths,
canonical root, creation time, state, and available `jj` and runtime metadata.

Pando owns workspace and runtime lifecycle plus command execution. Agent
planning, tool selection, model routing, and protocol adapters remain outside
Pando. An integration only needs to map execution to
`pando exec <name> -- <argv...>` and interactive access to
`pando shell <name>`.
