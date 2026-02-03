# Fuzzing Protocol

This project uses `cargo-fuzz` for long-running fuzz campaigns.

## Setup

1. Install `cargo-fuzz` (nightly Rust is required by cargo-fuzz):
   - `cargo install cargo-fuzz`
2. Ensure you have a recent Rust nightly toolchain installed:
   - `rustup install nightly`

## Targets

- `parser`: fuzzes the Markdown parser and serializer round-trip.
- `apply_changes`: fuzzes sync message application.
- `decode_changes`: fuzzes binary decoding of sync messages.

## Just Targets

Three fuzzing profiles are available via `just`:

| Target | Duration | Workers | Memory Limit | Use Case |
|--------|----------|---------|--------------|----------|
| `just fuzz-quick` | 5 min/target | 1 (single process) | 2GB | Quick sanity check |
| `just fuzz-moderate` | 1 hour/target | 15 | 2GB | Regular testing |
| `just fuzz-long-run` | 24 hours/target | 15 | 4GB | Overnight/weekend runs |

### Quick Run (15 minutes total)

```bash
just fuzz-quick
```

Runs each target for 5 minutes in single-process mode. Safe for any machine.

### Moderate Run (3 hours total)

```bash
just fuzz-moderate
```

Runs each target for 1 hour with 15 parallel workers. Good for CI or dedicated testing.

### Long Run (72 hours total)

```bash
just fuzz-long-run
```

Runs each target for 24 hours with 15 parallel workers. Intended for thorough fuzzing campaigns.

## Manual Runs

Run a single target with custom options:

```bash
cargo +nightly fuzz run parser -- -max_total_time=60 -rss_limit_mb=2048
```

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
   cargo +nightly fuzz run parser fuzz/artifacts/parser/crash-xxxxx
   ```
2. File a bug with the crash details.
3. Add a regression test if possible.

## Log Files

When using `-workers` and `-jobs`, LibFuzzer creates `fuzz-N.log` files in the working directory. These are gitignored and can be cleaned up with:

```bash
rm fuzz-*.log
```
