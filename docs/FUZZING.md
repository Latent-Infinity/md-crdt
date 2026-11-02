# Fuzzing Protocol

This project uses `cargo-fuzz` for long-running fuzz campaigns.

## Setup

1. Install the project-selected fuzz tooling:
   - `cargo install cargo-fuzz --version 0.13.2 --locked`
2. Install the tooling-only nightly:
   - `rustup toolchain install nightly-2026-10-01 --profile minimal`

## Targets

- `parser`: fuzzes bounded Markdown parsing and both Structural and Exact serialization.
- `apply_changes`: fuzzes sync buffers, semantic session application, and late cross-peer dependencies.
- `decode_changes`: fuzzes postcard change-message decoding and payload-opaque sync application.
- `decode_binary_wire`: fuzzes the binary envelope/change wire boundary.
- `merge_convergence`: fuzzes collaborative merge convergence.
- `snapshot_restore`: fuzzes bounded snapshot bytes and real parsed/materialized seeds with pending/deferred envelopes; checks stable restore, late dependency replay, checkpoint/rebase, and post-rebase editing.

## Just Targets

Three fuzzing profiles are available via `just`:

| Target | Duration | Workers | RSS limit per worker | Use case |
| --- | --- | --- | --- | --- |
| `just fuzz-quick` | 5 min/target | 1 (single process) | 2 GiB | Bounded local sanity check |
| `just fuzz-moderate` | 1 hour/target | 15 | 2 GiB | Manual host with sufficient memory headroom |
| `just fuzz-long-run` | 24 hours/target | 15 | 4 GiB | Long manual campaign |

### Quick Run (30 minutes total)

```bash
just fuzz-quick
```

Runs each target for 5 minutes in single-process mode. Select input and RSS limits
for the host; a 2 GiB per-process threshold is not safe on every machine.

### Moderate Run (6 hours total)

```bash
just fuzz-moderate
```

Runs each target for 1 hour with 15 parallel workers. The worker RSS thresholds
sum to 30 GiB, before coordinator/build overhead; this is not a shared process-group
memory cap. The project's CI remains paused.

### Long Run (144 hours total)

```bash
just fuzz-long-run
```

Runs each target for 24 hours with 15 parallel workers. Worker RSS thresholds sum
to 60 GiB before other overhead. Provision headroom or reduce workers/limits.

## Manual Runs

Run a single target with custom options:

```bash
cargo +nightly-2026-10-01 fuzz run parser -- -max_total_time=60 -rss_limit_mb=2048 -max_len=65536
```

The checked-in `fuzz/seeds/parser/` directory includes the multibyte-whitespace
frontmatter regression. Copy it into the ignored working corpus before a run:

```sh
mkdir -p fuzz/corpus/parser
cp fuzz/seeds/parser/* fuzz/corpus/parser/
cargo +nightly-2026-10-01 fuzz run parser -- -max_total_time=60 -rss_limit_mb=2048 -max_len=65536
```

The snapshot target constructs valid snapshot seeds through public APIs and
mutates their encoded bytes. The checked-in Markdown seed includes frontmatter,
marks, nested containers, a fence and a table. Seed a working corpus with:

```sh
mkdir -p fuzz/corpus/snapshot_restore
cp fuzz/seeds/snapshot_restore/* fuzz/corpus/snapshot_restore/
cargo +nightly-2026-10-01 fuzz run snapshot_restore -- -max_total_time=60 -rss_limit_mb=2048 -max_len=4096
```

Each iteration bounds input to 4 KiB, text generation to 256 input bytes, encoded
mutations to 16 positions per generated snapshot, and declared snapshot expansion
to 2 MiB. These fuzz-workload bounds do not change production reader limits.

When using `-runs=N`, inspect the observed `INITED` and `DONE` counters rather
than treating the requested count as evidence of mutation iterations. Replaying
a larger existing corpus can consume the run before new mutations begin.

### Key LibFuzzer Options

| Option | Description |
|--------|-------------|
| `-max_total_time=N` | Stop after N seconds |
| `-rss_limit_mb=N` | Kill if RSS memory exceeds N MB |
| `-max_len=N` | Maximum input size in bytes |
| `-workers=N` | Number of parallel fuzzer processes |
| `-jobs=N` | Total jobs to run (0 = single process mode) |

## Output

`cargo-fuzz` writes crash artifacts to `fuzz/artifacts/<target>/`.

If a crash occurs:

1. Save the artifact and run the reproducer:
   ```bash
   cargo +nightly-2026-10-01 fuzz run parser fuzz/artifacts/parser/crash-xxxxx
   ```
2. File a bug with the crash details.
3. Add a regression test if possible.

## Log Files

When using `-workers` and `-jobs`, LibFuzzer creates `fuzz-N.log` files in the working directory. These are gitignored and can be cleaned up with:

```bash
rm fuzz-*.log
```
