# Contributing

## Build and test

Build the host-only binaries:

```bash
cargo build --release
```

Build the Linux runtime-enabled binary:

```bash
cargo build --release --features microvm-boxlite
```

The binaries are written to `target/release/pando` and `target/release/pd`.
Run the default suite with `cargo test`. Integration tests in
`tests/jj_registration.rs` are skipped when `jj` is not on `PATH`. Live BoxLite
tests additionally require a supported hypervisor and are ignored by default.

## Benchmark workspace creation

Measure a checkout against a real source directory:

```bash
cargo bench --bench create -- /path/to/workspace \
  --samples 10 \
  --output target/bench-results/current.json
```

Compare multiple Pando checkouts against the same workload:

```bash
cargo bench --bench compare -- /path/to/workspace \
  --candidate /path/to/pando-baseline \
  --candidate /path/to/pando-candidate \
  --samples 10 \
  --output target/bench-results/compare-run
```

The create benchmark times only `create_workspace`. Cleanup runs after every
sample, and the JSON result records the median, minimum, maximum, and cleanup
status.
