# Third-party runtime notices

The Linux x86_64 Pando binary embeds the native runtime distributed with
BoxLite 0.9.7. Pando does not modify the native components. Pando's source tree
vendors the published BoxLite Rust crate with narrow patches that select Go's
native DNS resolver on Linux and omit an unused empty shared virtiofs device
from BoxLite's disk-rootfs path. The release attaches a
`boxlite-runtime-sources-<pando-version>.tar.gz` source bundle containing the exact
native source revisions listed below.

| Component | Embedded use | Source revision | License |
| --- | --- | --- | --- |
| BoxLite | guest agent and VMM shim | `boxlite-ai/boxlite` `8803834036205cf2cac5cfca98bb3875812c897a` (`v0.9.7`) | Apache-2.0 |
| bubblewrap | Linux jailer (`bwrap`) | `containers/bubblewrap` `9ca3b05ec787acfb4b17bed37db5719fa777834f` | LGPL-2.0-or-later |
| e2fsprogs | `mke2fs` and `debugfs` | `tytso/e2fsprogs` `da631e117dcf8797bfda0f48bdaa05ac0fbcf7af` | GPL-2.0; bundled libraries have their upstream licenses |
| libkrun | statically linked VMM | `boxlite-ai/libkrun` `e12b9b3780ffa8df9f3e1797b217d13453479167` | Apache-2.0 |
| libkrunfw | guest kernel/firmware library | `boxlite-ai/libkrunfw` `fad43a12d689586b4cb46110efc1d2a0f20b5361` | GPL-2.0-only and LGPL-2.1-only; see upstream file notices |

The corresponding license texts and per-file notices are included in those
source archives. The original projects and their histories are available from
the named GitHub repositories.
