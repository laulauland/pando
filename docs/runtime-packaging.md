# Linux runtime packaging

Pando 0.4's Linux x86_64 GitHub release and Homebrew bottle include the BoxLite
runtime. The same binary retains the ordinary host-only lifecycle: with no
configured runtime, `create` remains host-only, and `--no-runtime` is an explicit
override. macOS release artifacts are host-only; VM runtimes are supported only
on Linux for this release.

The release workflow downloads the BoxLite 0.9.7 Linux runtime archive once,
requires SHA-256
`9ae495f55d363e6af04640ab55025ac80b4bf4762e38fa0b8ac80c7604e3148c`,
and gives BoxLite a verified local `file://` URL. Linux source-formula builds use
the same pinned Homebrew resource. Release artifacts include
`THIRD_PARTY_NOTICES.md` and an exact-revision source bundle for the native
runtime components.

The Linux-only experimental workflow uses Rust 1.97.0 and full-SHA-pinned actions. It builds with `--locked --profile runtime-experimental --features microvm-boxlite`, smoke-tests the default and runtime-enabled binaries separately, emits a checksum beside each CI artifact, and never attaches those artifacts to a GitHub release. Each manually dispatched self-hosted Linux lane downloads that exact artifact, verifies its checksum, unpacks it, and passes its binary explicitly to the full lifecycle/crash harness. The custom unpublished profile retains debug assertions solely so the packaged qualification binary includes Pando's lifecycle crash-injection points.

Privileged lanes run only for a manual dispatch of the protected `main` ref and all target the `pando-runtime-qualification` GitHub environment. Repository administrators must configure that environment with required reviewers and a deployment-branch rule allowing only protected `main`; the YAML name alone does not create or secure the environment. Pull requests, pushes, unprotected refs, and arbitrary workflow-dispatch refs cannot enter these jobs. Their tokens have only `actions: read` and `contents: read`, and they consume artifacts produced by the same trusted workflow run.

Self-hosted labels are routing metadata, not a trust boundary. The KVM root and KVM rootless workers must be single-job ephemeral machines (or be fully reimaged after every job), contain no long-lived repository or cloud credentials, and accept jobs only from this repository/environment. Approval must be denied if the dispatch is not the expected protected `main` revision. `scripts/check-runtime-workflow-trust` runs a locked Rust checker that parses the workflow as YAML and semantically guards the exact branch/protection predicate, environment, runner-label set, permissions map, and artifact-download action for both lanes. Every `uses:` step must be an owner/repository action pinned to an exact 40-hex commit; mutable refs, malformed SHAs, local actions, and Docker action refs are rejected. Negative fixtures cover those forms as well as comment smuggling, `if: false`, write permissions, misplaced/wrong actions, and missing environments.

## Qualification matrix

| Platform | Build | Live workflow | Current evidence |
| --- | --- | --- | --- |
| Linux x86_64 with KVM API version 12 | Hosted CI | Self-hosted KVM, separately as root and non-root | gondor passes the complete workflow as non-root with fuse-overlayfs; the kernel-OverlayFS lane is defined but has not run |
| Apple Silicon macOS | Host-only release | None | VM runtime unsupported in 0.4 |
| Intel macOS | Host-only release | None | VM runtime unsupported in 0.4 |
| Other Linux architectures or Linux without usable KVM | Not supported | None | runtime creation reports the qualified architecture or `/dev/kvm` access requirement before workspace mutation |

The Linux runtime release requires the complete rootless fuse-overlayfs live lane
on the exact release candidate. Kernel-OverlayFS remains an additional qualified
lane rather than a claim required for rootless release support. Before workspace
mutation, Linux opens `/dev/kvm` and requires `KVM_GET_API_VERSION == 12`.

## Measured Linux footprint

Measured on gondor on 2026-07-19 with Rust 1.97.0, an already populated Cargo registry, and otherwise empty target directories:

| Build | Clean release time | `pando` bytes |
| --- | ---: | ---: |
| default features | 25.803 s | 17,271,264 |
| `microvm-boxlite` | 65.881 s | 104,208,512 |

The crash-qualified `runtime-experimental` Linux artifact measured 111,252,584 bytes. It is intentionally not a release-size measurement because debug assertions are retained for exact-artifact crash testing.

The runtime-enabled Linux executable dynamically needs only the system loader, glibc, libm, and libgcc_s. BoxLite embeds 66.2 MB of native/runtime assets into the executable: `boxlite-shim` (29,320,000 bytes), `libkrunfw.so.5` (19,203,768), `boxlite-guest` (14,480,904), `debugfs` (3,374,856), `mke2fs` (2,831,648), and `bwrap` (187,376), plus its compiled seccomp filter. They are extracted into BoxLite's runtime state when used.

## Pinning, provenance, and notices

Pando pins the pre-1.0 `boxlite` crate exactly to `0.9.7`; its API and serialized/provider behavior must be requalified before every version change. BoxLite, `libkrun-sys`, `e2fsprogs-sys`, and `bubblewrap-sys` declare Apache-2.0 crate metadata. The downloaded runtime also contains libkrun firmware, e2fsprogs programs, bubblewrap, a guest agent, and a shim, so crate metadata alone is not a sufficient binary notices inventory.

BoxLite 0.9.7's crate would ordinarily download its native runtime without a
Pando-controlled checksum. Pando's release and Linux source-formula paths instead
verify the archive before the crate consumes it. `THIRD_PARTY_NOTICES.md` records
the native components, licenses, and exact source revisions; every Linux release
also attaches those sources. The archive is still hosted by upstream BoxLite, so
a future mirror would improve availability without changing the verified bytes.

Neither Pando nor BoxLite 0.9.7 declares a Rust MSRV. The measured compiler
version is evidence, not an MSRV promise; release builds use the repository's
pinned Rust 1.97.0 toolchain until an explicit MSRV is established.

## Future executor adapter

A Pi or Codex integration should remain outside Pando core. Its narrow `WorkspaceExecutor` adapter can map a selected workspace to `pando exec <name> -- <argv...>` (and, when human interaction is required, `pando shell <name>`), while its brain retains planning, tool selection, and policy. Pando continues to own only workspace/runtime lifecycle and command execution; it should not learn agent protocols, model routing, or tool schemas.
