# Architecture Evolution — Plan State

Companion to [`architecture-evolution.md`](architecture-evolution.md).

| Field | Value |
| --- | --- |
| **Plan** | `docs/architecture-evolution.md` (Draft revision 6) |
| **Last updated** | 2026-07-10 |
| **Tracking unit** | PR slices under design phases A–H |

## Progress

| Unit | Status | Notes |
| --- | --- | --- |
| **PR-01** Codec + Envelope + DocOp DTOs + JsonOpCodec | **done** | `src/codec/`, `tests/codec_roundtrip.rs` (9 tests) |
| **PR-02** Deterministic BlockId | **done** | `block_id_from_op`, create paths, vault match adds; `tests/doc_block_id.rs` (5 tests) |
| **PR-03** CollaborativeDocument + apply_remote | **done** | `src/session/`, sync apply_one surface, `tests/session_collab.rs` (11 tests) |
| PR-04 SessionSnapshot | pending | Depends PR-03 |
| PR-05 SemanticConflict (optional) | pending | Not MVP gate |
| PR-06a/06b Text CRDT | pending | Phase B |
| PR-07 Marks | pending | Phase C |
| PR-08+ | pending | Product / polish |

## Phase A checklist (from plan)

- [x] `DocOp::InsertBlock` / `DeleteBlock` round-trip codec (string-mode: non-empty paragraph body allowed)
- [x] Two-peer test: concurrent block inserts via **session APIs** (PR-03)
- [x] `Operation.id` equals max id in envelope (session path, PR-03)
- [ ] Snapshot save/restore (PR-04)
- [ ] Snapshot size growth documented (PR-04)
- [x] Unknown wire version rejected without mutation
- [x] Fuzz-ready envelope decode fail closed (malformed JSON / depth / version)
- [ ] SemanticConflict optional (PR-05)
- [x] Deterministic BlockId from create OpId (PR-02)

## Decisions log (implementation)

| Decision | Choice | Rationale |
| --- | --- | --- |
| `MAX_WIRE_NEST_DEPTH` | **16** (plan sketch had 32) | Serde recursion vs depth walk |
| Empty-paragraph rejection | Session concern only | Codec predicate only |
| `DocOp` variants in PR-01 | InsertBlock + DeleteBlock only | Text ops later |
| `block_id_from_op` layout | `(peer << 64) \| counter` | KISS, no uuid v5 |
| `apply_one` vs `apply_changes` | `apply_one` does **not** auto-promote pending; `apply_changes` promotes for backward compat | Session interleaves document apply with `promote_ready_pending` |
| Table on wire in session | **Reject** `BlockKind::Table` with `SessionError::UnsupportedBlockKind("table")` (was: empty-RawBlock placeholder) | Placeholder silently dropped table content from the local doc + wire; fail-loud until the table PR (Phase E / PR-12) |
| Unit-mode local insert_block | Strips paragraph text on wire **and** document apply follows envelope | Body via future InsertText; matches N6-d |

## Gate

- `just check` — **passed** (fmt + clippy `-D warnings` + workspace tests) after PR-03 audit
- Session collab tests: **11 passed** (audit added 5: missing-anchor, missing-target, op-id-mismatch, peer-mismatch, table-reject)
- Sync apply_one tests: included in lib tests
- Audit fix: `insert_block(BlockKind::Table)` now returns `SessionError::UnsupportedBlockKind` instead of silently degrading to an empty RawBlock
