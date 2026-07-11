# Architecture Evolution — Plan State

Companion to [`architecture-evolution.md`](architecture-evolution.md).

| Field | Value |
| --- | --- |
| **Plan** | `docs/architecture-evolution.md` (Draft revision 6) |
| **Last updated** | 2026-07-11 |
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
| Nested collab parents (shipped with PR-09) | **done** | Feature-sized (not audit-only): `parent` on Insert/DeleteBlock, recursive apply, first-ingest quote tree. Prefer separate reviewed unit next time. |
| **Follow-up: nested re-ingest matching** | **done** | Recursive `sync_tree`; quote containers content-agnostic; structure add/remove inside quotes; stricter match floor |
| **PR-10** Vault ingest D2 | **done** | Grapheme LCS → InsertText/DeleteText; position-pair unmatched paragraphs; preserves BlockId + LCS unit OpIds |
| **PR-11** Headings and lists | **done** | Structured model; ATX/setext parse; ordered/unordered nested lists; canonical serialization; wire/snapshot round trips. Audit fixed silent list-ingest text loss (nested item children now ingested). |
| PR-12+ | pending | Tables, split/merge, … |

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
| Nested machinery packaging | Landed in same commit as PR-09 + plan-state | Prefer: feature commit (code+tests) then plan/docs commit, or a dedicated PR slice |
| Quote editing via vault | Structure re-ingest supported; in-quote *text* LCS still PR-10 | Nested structure add/remove/re-ingest preserve quote + matched child ids |
| Structure match floor | `min_match_score: 5000` for re-ingest | Default 2000 allowed content_sim=0 matches via position alone |
| Text ingest LCS | Visible graphemes; deletes high→low then inserts; batch insert runs | Preserves unit OpIds on LCS; marks on deleted units dropped (D non-goal) |
| Unmatched paragraph pairing | Zip remaining removed/added paragraphs by order | Lets full rewrite still keep BlockId via LCS |
| List indentation | Expand tabs to CommonMark four-column tab stops | Counting a tab as one character made indented list continuations normalize differently on the second parse; column expansion is deterministic and fixture-compatible |
| Structured serialization | Canonical ATX headings and `-` / `1.` list markers | The runtime model intentionally does not retain source marker style; structural serialization prioritizes stable semantics and idempotence |

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
- [x] Blockquotes preserved on **first** ingest (not flattened); tables skipped, no vault-wide abort
- [x] External in-paragraph text edit → InsertText/DeleteText (PR-10)
- [x] **Nested re-ingest matching** (structure + in-quote text LCS via PR-10)
- [ ] Two vaults exchange ops after external edits (later)

## Phase E checklist (partial)

- [x] ATX and setext headings parse to `Heading` with text units
- [x] Ordered, unordered, and nested lists parse to `List` / `ListItem`
- [x] List **ingest** preserves item children (text), not just the list container
- [x] Structured headings and lists serialize idempotently across CommonMark fixtures
- [x] Heading/list wire DTOs and session snapshots round-trip
- [ ] GFM table parsing and collaborative row operations (PR-12)
- [ ] Collaborative split/merge block operations (PR-13)

## Nested re-ingest matching — **done** (structure)

**Shipped:**
- Remove skip-on-blockquote-re-ingest
- Recursive `sync_tree(parent, old_children, new_children)` using level match
- Blockquotes fingerprinted as container token (`"blockquote"`) so child text changes do not destroy the container match
- Structure ops inside quotes via `delete_block_in` / `insert_paragraph_in` / `insert_block_in`
- Re-ingest match config `min_match_score: 5000` (avoids position-only false matches when content_sim=0)
- Tests: unchanged quote → NoOp; add/remove para inside quote preserves quote + matched sibling ids; text replace keeps quote id (leaf remove+add)

## PR-10 text LCS ingest — **done**

- `filesync/diff.rs`: grapheme LCS steps + helpers
- `apply_paragraph_text_diff`: DeleteText (high→low) then InsertText runs
- Position-pair residual paragraphs after content match so rewrites keep `BlockId`
- Works for top-level and nested (quote) paragraphs
- Marks on deleted units may drop (documented D limitation)

**Audit (verified):** `diff.rs` LCS traced correct (insert-middle, replace-all); deletes high→low keep offsets valid; equal units keep OpIds (test `..._preserves_block_and_prefix_unit_ids` asserts ≥2 shared unit ids retained on `hello`→`help`). **TDD gap closed:** added `ingest_full_paragraph_rewrite_preserves_block_id_via_position_pairing` (`alpha`→`zzzzz`, content_sim=0 below the 5000 floor → only position-pairing keeps the id). Matched code/raw blocks with changed content are left as-is (whole-string, not unit CRDTs) — minor documented limitation.

## PR-11 audit (headings and lists) — **done**

**Verified TRUE:** structured `Heading {level, text}` and `List {ordered, items: Sequence<ListItem>}` model; ATX/setext parse; ordered/unordered/nested list parse; canonical serialization (ATX `#`, `-`/`1.` markers) idempotent across fixtures; wire DTOs (`BlockKindSkeleton::Heading/List`, `ListItemSkeleton`) and snapshots round-trip; `insert_one(Heading)` uses N6-d (empty heading block + `insert_text` body).

**Bug found + fixed (list ingest silent data loss).** `insert_one(List)` inserted only the list block and dropped every item's children, so `- alpha\n- beta` ingested as a list whose items had no text (serialized `"-\n-"`). Fix mirrors the blockquote nested-text machinery:
- `insert_one(List)` now allocates contiguous session-peer item elem_ids (`base + 1 + i`), inserts the list with empty `ListItem`s, then inserts each item's children via `insert_tree(Some(item_elem), children)` (text flows through `InsertText`, N6-d). Item ids are self-peer → syncable, not parser-peer-0.
- `doc/mod.rs` recursive nav now descends `List → ListItem → children`: new `child_seqs` (used by `find_block`/`find_block_by_id`), `find_list_item`, and `with_container_children_mut` (used by `insert_block_at`/`delete_block_at`/`container_children`); `with_block_mut` descends into list items via `items.value_mut`.
- Tests: `ingest_preserves_list_structure_and_text` (text + structure preserved; re-ingest NoOp) and `nested_paragraph_in_list_item_converges` (insert list + nested paragraph + edit → converges across peers, state vectors equal).

**Latent limitation (documented, same as blockquotes):** in-list-item *text* re-ingest LCS not yet exercised end-to-end; nested-list matching relies on the level-based `sync_tree`.

## Gate

- `just check` — **passed** after PR-11 list-ingest fix (329 tests, 0 warnings)
- `cargo llvm-cov --workspace --all-features --summary-only` — **passed**; 85.93% repository line coverage, with all PR-11 follow-up lines covered (existing global baseline remains below 90%)
- Prior notes: nested re-ingest structure; multi-quote sibling edge still untested
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
