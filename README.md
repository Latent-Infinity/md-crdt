# md-crdt
md-crdt is a high-performance Rust library/CLI for offline-first collaborative Markdown editing. It merges changes deterministically with Markdown-aware blocks, frontmatter, and mark intervals, and syncs bidirectionally with plain .md files for true file ownership.

## CLI Quickstart

Install `just` (optional but recommended for project commands):

```sh
cargo install just
```

Run the CLI from the repo root:

```sh
cargo run -p md-crdt-cli -- status
```

Try it against a minimal vault:

```sh
mkdir -p /tmp/mdcrdt-demo/.mdcrdt/state
echo "hello" > /tmp/mdcrdt-demo/file1.md
cargo run -p md-crdt-cli -- status
```

JSON output:

```sh
cargo run -p md-crdt-cli -- status --json
```
