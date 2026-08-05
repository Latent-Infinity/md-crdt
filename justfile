# Justfile for md-crdt

# Set up development environment (run once after cloning)
setup:
    git config core.hooksPath .githooks
    @echo "Git hooks configured. Pre-commit hook will enforce formatting."

# Run all tests
test:
    cargo test --workspace

# Check formatting
fmt:
    cargo fmt -- --check

# Lint with clippy
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings -W clippy::perf -W clippy::needless_collect -W clippy::map_flatten -W clippy::or_fun_call -W clippy::inefficient_to_string -W clippy::unnecessary_wraps -W clippy::useless_conversion

# Run all quality checks
check: fmt lint test

# Differential testing against the naive oracle (both ordering strategies)
differential-test:
    PROPTEST_CASES=${PROPTEST_CASES:-100000} cargo test --test core_differential differential_test_sequence
    PROPTEST_CASES=${PROPTEST_CASES:-100000} cargo test --features sequence_incremental --test core_differential differential_test_sequence

# Run benchmarks
bench:
    cargo bench
    cargo bench --features sequence_incremental

# Competitive md-crdt vs Yrs harness tests (nested workspace; does not use --workspace)
test-compare:
    cargo test --manifest-path md-crdt-yrs-bench/Cargo.toml --locked

# Full competitive suite (cite only with multi-run provenance; see md-crdt-yrs-bench/README.md)
bench-compare:
    cargo bench --manifest-path md-crdt-yrs-bench/Cargo.toml --locked --bench compare

# Competitive liveness only — Criterion --test; never cite timings from this recipe
bench-compare-quick:
    cargo bench --manifest-path md-crdt-yrs-bench/Cargo.toml --locked --bench compare -- --test

# Three full competitive invocations + provenance sidecar (citable path; long-running)
bench-compare-report:
    md-crdt-yrs-bench/scripts/run_compare_report.sh

# Separate-process RSS probe (not Criterion; interpret engines independently)
memory-compare:
    md-crdt-yrs-bench/scripts/memory_rss.sh

# md-crdt exclusive-span sub-probes (requires sub_probes; not competitive ratios)
sub-probes:
    cargo run --manifest-path md-crdt-yrs-bench/Cargo.toml --example sub_probes --features sub_probes --release --locked

# Flamegraph recipe for competitive-shaped insert / apply (needs cargo-flamegraph)
# Example: just flamegraph-compare insert
#          just flamegraph-compare apply
flamegraph-compare target="insert":
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{target}}" in
      insert)
        echo "Profile: public insert path via sub_probes example (insert_middle)."
        echo "Install: cargo install flamegraph"
        cargo flamegraph --manifest-path md-crdt-yrs-bench/Cargo.toml \
          --features sub_probes --example sub_probes --release \
          -o md-crdt-yrs-bench/report-out/flamegraph-insert.svg -- insert
        echo "Also: samply record cargo run --manifest-path md-crdt-yrs-bench/Cargo.toml --example sub_probes --features sub_probes --release -- insert"
        ;;
      apply)
        echo "Profile apply_remote k=100 via the same example (second pair of tables)."
        cargo flamegraph --manifest-path md-crdt-yrs-bench/Cargo.toml \
          --features sub_probes --example sub_probes --release \
          -o md-crdt-yrs-bench/report-out/flamegraph-apply.svg -- apply
        ;;
      *)
        echo "usage: just flamegraph-compare insert|apply" >&2
        exit 1
        ;;
    esac

# Fetch external markdown test fixtures (markdown-it, Comrak, GFM spec)
fuzz-fetch-fixtures:
    python3 scripts/fetch_test_fixtures.py

# Seed fuzz corpus with markdown from test fixtures
fuzz-seed:
    python3 scripts/seed_fuzz_corpus.py

# Fetch fixtures and seed corpus
fuzz-init: fuzz-fetch-fixtures fuzz-seed

# Quick fuzz run (5 minutes per target, single process, no worker spawning)
fuzz-quick:
    cargo +nightly fuzz run parser -- -max_total_time=300 -rss_limit_mb=2048 -max_len=65536
    cargo +nightly fuzz run apply_changes -- -max_total_time=300 -rss_limit_mb=2048 -max_len=65536
    cargo +nightly fuzz run decode_changes -- -max_total_time=300 -rss_limit_mb=2048 -max_len=65536
    cargo +nightly fuzz run merge_convergence -- -max_total_time=300 -rss_limit_mb=2048 -max_len=4096

# Moderate fuzz run (1 hour per target, 15 workers)
fuzz-moderate:
    cargo +nightly fuzz run parser -- -max_total_time=3600 -rss_limit_mb=2048 -max_len=65536 -jobs=15 -workers=15
    cargo +nightly fuzz run apply_changes -- -max_total_time=3600 -rss_limit_mb=2048 -max_len=65536 -jobs=15 -workers=15
    cargo +nightly fuzz run decode_changes -- -max_total_time=3600 -rss_limit_mb=2048 -max_len=65536 -jobs=15 -workers=15
    cargo +nightly fuzz run merge_convergence -- -max_total_time=3600 -rss_limit_mb=2048 -max_len=4096 -jobs=15 -workers=15

# Run long fuzzing campaign (manual, use with caution)
# 15 workers with strict memory limits per worker
fuzz-long-run:
    cargo +nightly fuzz run parser -- -max_total_time=86400 -rss_limit_mb=4096 -max_len=65536 -jobs=15 -workers=15
    cargo +nightly fuzz run apply_changes -- -max_total_time=86400 -rss_limit_mb=4096 -max_len=65536 -jobs=15 -workers=15
    cargo +nightly fuzz run decode_changes -- -max_total_time=86400 -rss_limit_mb=4096 -max_len=65536 -jobs=15 -workers=15
    cargo +nightly fuzz run merge_convergence -- -max_total_time=86400 -rss_limit_mb=4096 -max_len=4096 -jobs=15 -workers=15

# Generate code coverage report
coverage:
    cargo llvm-cov
