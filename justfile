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

# Generate code coverage report
coverage:
    cargo llvm-cov
