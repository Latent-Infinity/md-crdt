# Architecture Evolution — Plan State

Companion to [`architecture-evolution.md`](architecture-evolution.md).

| Field | Value |
| --- | --- |
| **Plan** | `docs/architecture-evolution.md` (Draft revision 6) |
| **Last updated** | 2026-07-09 |
| **Tracking unit** | PR slices under design phases A–H |

## Progress

| Unit | Status | Notes |
| --- | --- | --- |
| **PR-01** Codec + Envelope + DocOp DTOs + JsonOpCodec | **done** | `src/codec/`, `tests/codec_roundtrip.rs` (9 tests), CHANGELOG Unreleased |
| PR-02 Deterministic BlockId | pending | Parallel-safe with PR-01 |
| PR-03 CollaborativeDocument + apply_remote | pending | Depends PR-01, PR-02 |
| PR-04 SessionSnapshot | pending | Depends PR-03 |
| PR-05 SemanticConflict (optional) | pending | Not MVP gate |
| PR-06a/06b Text CRDT | pending | Phase B |
| PR-07 Marks | pending | Phase C |
| PR-08+ | pending | Product / polish |

## Phase A checklist (from plan)

- [x] `DocOp::InsertBlock` / `DeleteBlock` round-trip codec (string-mode: non-empty paragraph body allowed)
- [ ] Two-peer test: concurrent block inserts via **session APIs** (PR-03)
- [ ] `Operation.id` equals max id in envelope (session path, PR-03)
- [ ] Snapshot save/restore (PR-04)
- [ ] Snapshot size growth documented (PR-04)
- [x] Unknown wire version rejected without mutation
- [x] Fuzz-ready envelope decode fail closed (malformed JSON / depth / version)
- [ ] SemanticConflict optional (PR-05)

## Decisions log (implementation)

| Decision | Choice | Rationale |
| --- | --- | --- |
| `MAX_WIRE_NEST_DEPTH` | **16** (plan sketch had 32) | Serde default recursion limit (~128) is hit by deeper nested JSON enum wrappers before a depth-32 check can run; 16 still exceeds real markdown quote depth and is enforceable on encode + decode |
| Empty-paragraph rejection | Session concern only | Codec exposes `insert_block_paragraph_is_empty`; no unconditional decode ban (string-mode / historical payloads) |
| `DocOp` variants in PR-01 | InsertBlock + DeleteBlock only | InsertText/DeleteText deferred to text CRDT work; KISS |
| `BlockKindSkeleton` variants in PR-01 | Paragraph, CodeFence, BlockQuote, RawBlock | `Table` deferred to Phase E / PR-12 (rows likely separate ops); plan sketch updated to match |
| Empty-paragraph error type | `SessionError::NonEmptyParagraphOnInsertBlock` (not `CodecError`) | Codec-agnostic rule kept out of per-codec `Error` types (Decision D pluggability); codec exposes only the `insert_block_paragraph_is_empty` predicate |

## Gate

- `just check` — **passed** (fmt, clippy, workspace tests)
- Codec integration tests: **9 passed**
- Incidental fix: `filesync` jaccard score uses `checked_div` (clippy `manual_checked_ops` on current toolchain)
