# Contributing to md-crdt

Thank you for your interest in contributing to md-crdt! This document provides guidelines and information for contributors.

## Code of Conduct

This project adheres to the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md). By participating, you are expected to uphold this code. Please report unacceptable behavior to security@latentinfinity.com.

## Getting Started

### Prerequisites

- Rust stable (edition 2024)
- [just](https://github.com/casey/just) command runner (optional but recommended)

### Building

```bash
# Clone the repository
git clone https://github.com/latenty-infinity/md-crdt.git
cd md-crdt

# Build all crates
cargo build --workspace

# Run tests
cargo test --workspace
```

### Using Just

If you have `just` installed, you can use these convenient commands:

```bash
just setup    # Set up git hooks (run once after cloning)
just fmt      # Check code formatting
just lint     # Run clippy
just test     # Run all tests
just check    # Run fmt + lint + test
```

**Important**: Run `just setup` after cloning to enable pre-commit hooks that enforce formatting.

## Development Guidelines

### Code Style

- Follow standard Rust formatting (`cargo fmt`)
- Pass all clippy lints (`cargo clippy --workspace -- -W clippy::perf`)
- Use meaningful variable and function names
- Prefer `impl Trait` return types for iterators to enable lazy evaluation
- Avoid unnecessary allocations (prefer `&str` over `String` where possible)

### Testing Requirements

We maintain high test coverage standards:

- **Unit tests**: Required for all new functionality
- **Property-based tests**: Use `proptest` for testing invariants
- **Differential tests**: Compare against the naive oracle for CRDT operations
- **Integration tests**: Required for cross-module functionality

Run the full test suite before submitting:

```bash
cargo test --workspace
```

### Performance Considerations

- Avoid O(n) operations where O(1) is possible
- Use `with_capacity()` when the size is known
- Prefer borrowing over cloning
- Use `std::mem::take()` to move values without cloning
- Run benchmarks for performance-critical changes:

```bash
cargo bench
```

### Commit Messages

- Use clear, descriptive commit messages
- Start with a verb in imperative mood (e.g., "Add", "Fix", "Update")
- Keep the first line under 72 characters
- Reference issues when applicable (e.g., "Fixes #123")

Example:
```
Add property tests for MarkSet convergence

- Add proptest strategies for MarkInterval generation
- Verify commutativity and idempotency properties
- Add differential tests against naive oracle

Fixes #42
```

## Submitting Changes

### Pull Request Process

1. **Fork** the repository and create a feature branch
2. **Write tests** for your changes
3. **Ensure all tests pass**: `cargo test --workspace`
4. **Run lints**: `cargo clippy --workspace`
5. **Format code**: `cargo fmt`
6. **Update documentation** if needed
7. **Submit a pull request** with a clear description

### Pull Request Guidelines

- Keep PRs focused on a single change
- Include tests for new functionality
- Update CHANGELOG.md for user-facing changes
- Ensure CI passes before requesting review

### Review Process

- All PRs require at least one approving review
- Address review feedback promptly
- Squash commits before merging if requested

## Project Structure

```text
md-crdt/
├── src/                # md-crdt library modules: core, doc, sync, storage, filesync
├── tests/              # Integration, property, differential, and fixture-based tests
├── md-crdt-cli/        # Command-line interface workspace crate
├── md-crdt-ffi/        # Foreign function interface workspace crate
├── md-crdt-naive-oracle/ # Reference implementation for differential testing
├── md-crdt-ci/         # CI utilities
└── fuzz/               # Fuzz testing targets
```

## Testing

### Running Tests

```bash
# All tests
cargo test --workspace

# Library crate only
cargo test -p md-crdt

# Property tests with more cases
PROPTEST_CASES=10000 cargo test --workspace

# Differential tests
cargo test --test core_differential differential_test_sequence
```

### Fuzzing

We use cargo-fuzz for fuzz testing:

```bash
# Install cargo-fuzz (requires nightly)
cargo +nightly install cargo-fuzz

# Run a fuzz target
cd fuzz
cargo +nightly fuzz run parser -- -max_total_time=60
```

See [docs/FUZZING.md](docs/FUZZING.md) for detailed fuzzing instructions.

## Reporting Issues

### Bug Reports

When reporting bugs, please include:

- Rust version (`rustc --version`)
- Operating system
- Steps to reproduce
- Expected vs actual behavior
- Minimal reproduction case if possible

### Feature Requests

For feature requests, please:

- Check existing issues first
- Describe the use case
- Explain why existing functionality doesn't suffice

## License

By contributing to md-crdt, you agree that your contributions will be licensed under the MIT License.

## Questions?

If you have questions, feel free to:

- Open a [GitHub Discussion](https://github.com/latenty-infinity/md-crdt/discussions)
- Check existing issues and documentation

Thank you for contributing!
