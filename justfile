# Justfile for md-crdt

# Run all tests
test:
    cargo test

# Check formatting
fmt:
    cargo fmt -- --check

# Lint with clippy
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# Run all quality checks
check: fmt lint test

# Differential testing against the naive oracle
differential-test:
    PROPTEST_CASES=${PROPTEST_CASES:-100000} cargo test -p md-crdt-core differential_test_sequence

# Run benchmarks
bench:
    cargo bench

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
