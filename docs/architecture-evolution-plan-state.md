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
| **PR-08** VaultSession | **done** | Path→CollaborativeDocument map; `.mdcrdt/peer_id`; session snapshots under `.mdcrdt/sessions/` |
| **PR-09** Vault ingest D1 | **done** | Hash gate + match_blocks → Insert/Delete block; N6-d for new paragraphs; `IngestReport` |
| PR-10+ | pending | Text LCS ingest D2, etc. |

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
| Session snapshot dir | `.mdcrdt/sessions/<rel>.mdcrdt` (not `.mdcrdt/state/`) | Avoid collision with existing fingerprint `LastFlushedState` blobs in `state/` until ingest unifies storage |
| Peer id format | Decimal `u64` text in `.mdcrdt/peer_id`; non-zero | Shared clock domain for all file sessions in a vault |
| Ingest D1 scope | Structure only (add/remove); matched different text deferred to D2 | Ship block ops without grapheme LCS |
| Ingest new paragraphs | `insert_paragraph` (N6-d) | Empty InsertBlock + InsertText body |
| Pure reorder | Match preserves BlockIds; no move op yet | CRDT order may lag file order until move/reorder support |

## Phase C checklist

- [x] One public `MarkSet` (rich causal)
- [x] `Block.marks` / `EditOp` use rich types
- [x] Generic LWW mark types removed from `core`
- [x] `render_spans` over paragraph unit order (`Document::render_paragraph_spans`)
- [x] Expand-on-insert / range-remove helpers still green

## Phase D checklist (partial)

- [x] Multi-file vault opens distinct sessions per path; shared peer id (PR-08)
- [x] Open/save per-file session snapshots (PR-08)
- [x] Structure ingest: hash gate → match → Insert/Delete block → snapshot (PR-09)
- [x] Block reorder preserves ids via match_blocks (PR-09; no CRDT re-order yet)
- [x] Blockquotes preserved on ingest (not flattened); tables skipped, no vault-wide abort (audit)
- [ ] External in-paragraph text edit → InsertText/DeleteText (PR-10)
- [ ] Nested re-ingest matching (blockquote edits preserving identity) — follow-up
- [ ] Two vaults exchange ops after external edits (later)

## Audit finding (PR-09) — ingest mishandled non-flat blocks → **fixed (nested editing machinery)**

**Symptom.** A blockquote in a file was **silently flattened** (structure lost on ingest→flush); a table in any file made `ingest_all` **abort the whole vault** (`?` on `UnsupportedIngestBlock`). Both untested.

**Fix (user chose "build nested-text machinery now").** Added collaborative nested block/paragraph editing:
- `DocOp::InsertBlock`/`DeleteBlock` gain `parent: Option<OpId>` (container elem_id; `None` = top-level; `#[serde(default)]` for back-compat). Apply navigates to the container's `children` via a recursive elem_id search (arbitrary depth). `InsertText`/`DeleteText` need no wire change — their apply now resolves `block_elem` recursively.
- Session `insert_block_in` / `insert_paragraph_in(parent, …)`; `insert_text`/`delete_text` locate blocks recursively (`Document::find_block`/`with_block_mut`/`container_children`).
- Ingest: first ingest inserts the parsed tree recursively (blockquotes preserved, syncable); tables → per-file **skip** (`IngestReport.files_skipped`), no abort. Table parsing isn't implemented yet, so the table path is defensive until PR-12.
- **Scope note:** nested *re-ingest* matching (editing inside a quote across re-ingests, preserving identity) is deferred — a quote file on re-ingest is skipped rather than flattened.
- Tests: `nested_paragraph_in_blockquote_converges` (session), `ingest_preserves_blockquote_structure` (vault).

## Gate

- `just check` — **passed** after PR-09 audit + nested editing (309 tests, 0 warnings)
- Prior audit notes retained (span-aware sync; mark visible-order split)


## Audit blocker (PR-06a) — text-unit / op-counter collision — **RESOLVED (span-aware sync)**

**Symptom (fixed).** Two string-mode paragraphs produced a duplicate OpId (`{2,1}` was both a block elem_id and a paragraph unit id). Regression test `string_mode_paragraph_units_do_not_collide_with_block_ids` (`tests/session_collab.rs`) now **passes**.

**Root cause.** Paragraph bodies expand into per-grapheme `TextUnit`s whose ids live in the same peer OpId counter space as operations, but that space was accounted for inconsistently: `insert_block` reserved only 1 counter for a G-unit paragraph, and `snapshot::max_counter_for_peer` skipped paragraph units.

**Fix — span-aware sync.** An `Operation` now covers a contiguous counter *range* `[e - span + 1, e]` where `e = id.counter`:
- `Operation.id` is the **max embedded id** (N1): a block at `b` with a G-grapheme paragraph has `id = {b+G}`, `span = G+1`. The block elem_id stays `{b}` (payload-internal).
- `SyncState::apply_one(op, span)` / `promote_ready_pending` gate on the range **start** vs the peer frontier (`start > frontier+1 → buffer`). Backward-compatible: `span == 1` reduces to the old contiguous check. `apply_changes` (legacy batch) uses span 1.
- The session computes span from the envelope (`operation_extent` / `max_counter_in_kind`) on send, receive, and pending-restore; `Operation` stays `{id, payload}` on the wire (receiver recomputes span).
- Side effect: snapshot `next_counter` recovery is now correct because the op log keys already encode `b+G` (the max), so `max_counter_for_peer` reads it from the ops.
- Also unblocks multi-unit `InsertText` paste (PR-06b), which needs the same range semantics.
