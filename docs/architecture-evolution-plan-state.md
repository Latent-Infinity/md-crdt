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
| **PR-01** Codec | **done** | |
| **PR-02** Deterministic BlockId | **done** | |
| **PR-03** CollaborativeDocument | **done** | |
| **PR-04** SessionSnapshot | **done** | |
| PR-05 SemanticConflict (optional) | **skipped for now** | Not MVP gate; can land later |
| **PR-06a** Paragraph TextUnit representation | **done** | `doc/text.rs`, parse/serialize/insert_text, snapshot v2. Span-aware sync for unit OpIds. |
| **PR-06b** Text CRDT wire ops | **done** | InsertText/DeleteText + insert_paragraph N6-d; unit_mode default true; with_value_mut |
| **PR-07** Marks | **done** | Unified rich `mark::MarkSet` on Block/EditOp; generic dual API removed |
| PR-08+ | pending | |

## Phase B checklist

- [x] In-memory paragraph as `Sequence<TextUnit>` (PR-06a)
- [x] Parse + serialize round-trip on units
- [x] insert_text inserts grapheme units (local API)
- [x] Snapshot format v2 + v1 string upgrade path
- [x] Wire InsertText/DeleteText + session commits (PR-06b)
- [x] Concurrent multi-peer paragraph edits (PR-06b)

## Decisions log (implementation)

| Decision | Choice | Rationale |
| --- | --- | --- |
| PR-05 before 06a? | **Skip optional PR-05** | MVP gate is 01–04 + 06a/b + 07 |
| Unit OpIds on skeleton expand | `parent_elem.counter + 1 + i` same peer | Deterministic across peers; wire still only carries block id + string |
| Snapshot paragraph body | v2 `units` list; accept v1 `text` string | Offline upgrade without live N6-c |
| Span-aware sync (audit) | `Operation` covers a counter range `[e-span+1, e]`; `Operation.id` = max embedded id (N1); readiness gates on range start vs frontier | Lets one op reserve a contiguous id range (block + G units) without sparse op ids; span 1 = old behavior; unblocks multi-unit InsertText |
| Unit-mode default | `CollaborativeDocument::new` → `unit_mode = true` | Phase B cutover; string-mode still available via `with_codec(..., false)` |
| N6-d insert_paragraph | empty `InsertBlock` then `InsertText` (two commits) | Pure N1–N4; no skeleton range-seed |
| Nested text apply | `Sequence::with_value_mut` on block element | Avoid full `Block` clone per unit |
| Mark unification | Keep rich causal `core::mark::MarkSet`; delete generic `MarkSet<K,V>` / `TextAnchor` | Single public API; matches `render_spans` + text-unit anchors |
| Deprecated aliases | `RichMarkSet` / `RichMarkInterval` type aliases for one release | Migration path for callers |

## Phase C checklist

- [x] One public `MarkSet` (rich causal)
- [x] `Block.marks` / `EditOp` use rich types
- [x] Generic LWW mark types removed from `core`
- [x] `render_spans` over paragraph unit order (`Document::render_paragraph_spans`)
- [x] Expand-on-insert / range-remove helpers still green

## Gate

- `just check` — **passed** after PR-07 audit (290 tests, 0 warnings)
- Prior PR-06b audit notes retained: span-aware sync + multi-unit paste convergence
- **Audit fix (PR-07):** `mark_ops::lower_remove_mark_range` ordered mark-split anchors by raw OpId (`anchor_lt`), which is wrong when a paragraph's units were concurrently interleaved (RGA orders same-anchor siblings by descending id, so OpId order ≠ visible order). Now compares by **visible position** (`resolve_anchor` over the paragraph's element order, threaded in from `Document::remove_mark`); removed the dead `anchor_lt`/`bias_rank`. Regression test `remove_mark_range_orders_by_visible_position_not_opid` (`doc/mod.rs`).

## Audit blocker (PR-06a) — text-unit / op-counter collision — **RESOLVED (span-aware sync)**

**Symptom (fixed).** Two string-mode paragraphs produced a duplicate OpId (`{2,1}` was both a block elem_id and a paragraph unit id). Regression test `string_mode_paragraph_units_do_not_collide_with_block_ids` (`tests/session_collab.rs`) now **passes**.

**Root cause.** Paragraph bodies expand into per-grapheme `TextUnit`s whose ids live in the same peer OpId counter space as operations, but that space was accounted for inconsistently: `insert_block` reserved only 1 counter for a G-unit paragraph, and `snapshot::max_counter_for_peer` skipped paragraph units.

**Fix — span-aware sync.** An `Operation` now covers a contiguous counter *range* `[e - span + 1, e]` where `e = id.counter`:
- `Operation.id` is the **max embedded id** (N1): a block at `b` with a G-grapheme paragraph has `id = {b+G}`, `span = G+1`. The block elem_id stays `{b}` (payload-internal).
- `SyncState::apply_one(op, span)` / `promote_ready_pending` gate on the range **start** vs the peer frontier (`start > frontier+1 → buffer`). Backward-compatible: `span == 1` reduces to the old contiguous check. `apply_changes` (legacy batch) uses span 1.
- The session computes span from the envelope (`operation_extent` / `max_counter_in_kind`) on send, receive, and pending-restore; `Operation` stays `{id, payload}` on the wire (receiver recomputes span).
- Side effect: snapshot `next_counter` recovery is now correct because the op log keys already encode `b+G` (the max), so `max_counter_for_peer` reads it from the ops.
- Also unblocks multi-unit `InsertText` paste (PR-06b), which needs the same range semantics.
