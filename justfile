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

# Generate code coverage report
coverage:
    cargo llvm-cov
