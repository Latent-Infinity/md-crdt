# Architecture Evolution — Plan State

Companion to [`architecture-evolution.md`](architecture-evolution.md).

| Field | Value |
| --- | --- |
| **Plan** | `docs/architecture-evolution.md` (Draft revision 6) |
| **Last updated** | 2026-07-12 |
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
| **PR-12** Table parse and row ops | **done** | GFM parse/alignment; table block skeleton; Insert/Set/Delete row DocOps; concurrent row convergence |
| **PR-13** SplitBlock / MergeBlocks | **done** | Atomic text-unit transfer for paragraph/heading siblings; nested APIs; identity-preserving split/merge with collision fallback |
| **PR-14** Block index / cached state vector / shared payloads | **done** | Nested BlockId path index; O(peers) state vector; `Arc<[u8]>` operation payloads; Criterion before/after |
| **PR-15** Incremental sequence order | **done** | Sibling-local insertion behind default-off `sequence_incremental`; debug dual-path check; top-level/nested Criterion results |
| **PR-16** Storage V2 generation protocol | **done** | Frozen V1 dual-read fixture; V2 CRC trailer; alternating metadata/payload slots; file + directory sync; crash fallback |
| PR-17+ | pending | Bench consolidation, module splits, FFI/README honesty, … |

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
| Table creation wire shape | `InsertBlock` carries header + column alignment only; rows use separate row DocOps | Preserves independent row identity and RGA concurrency without embedding mutable row history in a block skeleton |
| Table cell granularity | Whole-cell `String` through the existing LWW register | PR-12 calls for row operations; per-cell text CRDT would expand scope beyond the existing model and belongs in a separately designed slice |
| GFM delimiter recognition | Header count must match delimiter count; delimiter cells require at least three hyphens with optional edge colons | Matches the core GFM table shape while keeping parsing deterministic and dependency-free |
| Split/merge wire shape | Atomic operation carries explicit source/destination unit ids and graphemes | Composing DeleteText + InsertBlock + InsertText always renumbers units and breaks mark anchors; persistent unit ownership would exceed this slice |
| Merge id collisions | Preserve source unit ids unless the left sequence already retains the id (for example after split); allocate fresh contiguous ids only for collisions | Sequence tombstones intentionally prevent resurrection under an existing id; selective fallback preserves all identities that remain valid |
| Split/merge block kinds | Paragraphs and headings; split retains heading level and merge retains the left kind | These are the existing text-unit block kinds; code/raw/table/list semantics require separately designed operations |
| Block index invalidation | Public `IndexedBlocks` wrapper tracks a mutation generation; lookups validate cached paths and self-repair after direct field replacement | Preserves the existing `document.blocks.*` source shape while preventing stale-index results; clone/equality ignore cache state |
| Block index path shape | `BlockId` and `elem_id` map to container paths through blockquotes/list items | Raw references cannot survive sequence mutation safely; paths make lookup O(depth), bounded by structural depth rather than document size |
| Payload sharing ablation | `Arc<[u8]>` over `Bytes` | Both make clone O(1); `Arc` adds no dependency and payload slicing is unused. Serde `rc` preserves the existing byte-array JSON shape. |
| State-vector cache | Store the per-peer maximum beside the op map and update it on every applied/restore path | Append-only applied ops make invalidation unnecessary; `state_vector()` becomes O(peers) clone instead of O(ops) rescan |
| Sequence ordering ablation | Default full rebuild vs feature-gated sibling-local vector insertion | At 10k elements the incremental path reduced top-level insertion from 2.074 ms to 96.65 µs and nested text insertion from 2.165 ms to 96.69 µs; the flag stays default off for soak |
| Run-length text | **Defer** | Incremental ordering removed about 95.4% of the measured insertion cost; 10k structural serialization was already 17.57 µs. No profile justified storage/wire/mark complexity or any OpId representation change |
| V2 snapshot payload layout | Pair `segment_a`/`segment_b` with superblock A/B | A single replaced `segment` invalidates the older checksum, so metadata-only alternation cannot recover a previous generation. Two payload slots cost up to one extra active snapshot but make the crash/corruption fallback real |
| Durability boundary | Atomic temp write + file sync + rename + directory sync on Unix; same atomic sequence without directory sync elsewhere | Rust's portable filesystem API cannot guarantee directory fsync on every platform; the documented crash-safety claim is therefore explicit about the Unix durability boundary |

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
- [x] GFM table parsing with left/center/right alignment
- [x] Table block metadata and row insert/update/delete operations round-trip on the wire
- [x] Concurrent table-row inserts converge; row updates use existing LWW semantics
- [x] Collaborative split/merge block operations (PR-13)

## Phase F checklist (partial)

- [x] Criterion baselines: BlockId lookup, state vector, and change encoding (PR-14)
- [x] BlockId/elem-id nested path index with mutation invalidation (PR-14)
- [x] Cached state vector / per-peer applied frontier (PR-14)
- [x] Shared immutable operation payloads; persistence converts to owned bytes only at snapshot boundary (PR-14)
- [x] Incremental sequence ordering behind a soakable, default-off flag (PR-15)
- [x] Run-length text evaluated and deferred because profiling did not demand it (PR-15)

## Phase G checklist

- [x] Dedicated V1 decoder exercised by frozen pre-V2 bytes
- [x] V2 body uses generation metadata plus a little-endian CRC32 trailer
- [x] Writes alternate a single metadata/payload slot and preserve the previous generation
- [x] Payload and superblock temp files are synced before rename; containing directory is synced on Unix
- [x] Missing/corrupt newest metadata and interrupted payload publication fall back to the prior valid generation

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

## PR-12 tables — **done**

- Parser recognizes GFM header/delimiter pairs, validates three-or-more-hyphen delimiters, maps edge colons to alignment, and materializes ordered `TableRow`s.
- `BlockKindSkeleton::Table` carries immutable creation metadata (columns + header); non-empty runtime tables must be built through row operations.
- `DocOp::{InsertTableRow,SetTableRowCells,DeleteTableRow}` integrate through the session with N3 encode-before-apply and N4 `right_origin` on row insertion.
- Tests cover structured parsing, invalid delimiters, canonical round-trip, block/row codec round-trips, no-clock-burn validation, sequential row mutation, and concurrent row insertion convergence.
- Deliberate limit: escaped-pipe tokenization and inline-mark parsing inside cells are not implemented; vault ingest continues to skip table files until table diff semantics are designed.

**Audit (verified):** parse/serialize/wire/session/snapshot all trace correct; convergence + clock-safety tests present. **TDD gap closed:** snapshot had full `table_to_dto`/`table_from_dto` + row RGA/LWW DTOs and a `max_counter_for_peer` table arm, but **no test drove a table through save/restore**. Added `save_restore_round_trip_preserves_table_rows` (`tests/session_snapshot.rs`): table + two rows + cell update → snapshot → restore asserts serialize equality, equal state vectors, recovered `next_counter`, and a non-colliding post-restore row id. **Doc nit fixed:** `filesync/session.rs` skip comment reworded — tables *are* wire-ready; ingest simply doesn't yet emit `InsertBlock(empty)+InsertTableRow` from a parsed multi-row table (skip stays the correct scope boundary).

## PR-13 collaborative split/merge — **done**

- `DocOp::SplitBlock` transfers an explicit visible suffix into a new sibling, retaining text-unit ids and the paragraph/heading kind.
- `DocOp::MergeBlocks` uses a stable left-body anchor, appends the right sibling's visible units, merges mark history, and tombstones the right block atomically.
- Top-level and nested session APIs validate sibling membership, adjacency, text-bearing kinds, and grapheme offsets before allocating a clock id.
- Split→merge detects ids retained as tombstones in the left sequence and assigns fresh contiguous ids only to those collisions; ordinary merges preserve every right-side unit id.
- Tests cover codec round trips, ID preservation, collision fallback, nested headings, validation without clock burn, malicious replacement-peer rejection, multi-peer convergence, and snapshot/clock recovery.
- Deliberate limit: code fences, raw blocks, tables, lists, and blockquotes are rejected; their split/merge semantics are not equivalent to text-unit movement.

**Audit (verified):** split/merge traced correct — `operation_extent(MergeBlocks)` reserves the replacement-id counter range so span-aware sync stays contiguous; `check_peer_consistency` rejects foreign replacement ids; split reuses preserved suffix ids in a *separate* sequence (no collision); nested APIs recurse through `container_children`. **Coverage gap closed:** every prior "converges" test was one-directional (A acts → B applies), leaving true concurrency unproven. Added `concurrent_splits_of_same_block_converge` (`tests/session_split_merge.rs`): two peers split the same paragraph at different offsets, exchange both ways → identical structural serialization + equal state vectors (original keeps the common prefix, both suffixes survive as siblings). **Forward-looking note (not a shippable bug):** `SplitBlock` clones the whole `MarkSet` to the new block and `MergeBlocks` uses `MarkSet::merge_from`, but `DocOp` carries no mark ops and session blocks are always created with `MarkSet::new()`, so these paths only ever act on empty sets today; when a mark wire op lands, split must partition intervals at the boundary (a straddling mark would otherwise render spurious spans in both halves via `resolve_anchor`'s `unwrap_or(0)`).

## PR-14 indexed lookup and sync caches — **done**

- `Document` lazily builds `BlockId` and element-id maps to bounded-depth container paths; successful lookup is independent of document length after the first build.
- `IndexedBlocks` invalidates by mutation generation. Lookups validate their target and rebuild on a cache miss/mismatch, covering direct public sequence replacement without returning stale data.
- `SyncState` stores the applied per-peer frontier and updates it through `apply_op`, `apply_one`, pending promotion, local add, and snapshot restore.
- `Operation.payload` and the applied log use `Arc<[u8]>`; `encode_changes_since`, pending, and outbox clones share allocation. Snapshot APIs deliberately retain `Vec<u8>` as the owned persistence boundary.
- Differential/oracle sync tests remain green. Index tests cover top-level, blockquote, list-item, deletion, mutation invalidation, direct sequence replacement, clone/equality, and `Document: Send + Sync`.

### Criterion before/after (20 samples, 1 s measurement)

| Benchmark | Before | After | Change |
| --- | ---: | ---: | ---: |
| BlockId lookup, 1k blocks | 1.219 µs | 26.94 ns | -97.8% |
| BlockId lookup, 10k blocks | 14.109 µs | 33.53 ns | -99.8% |
| State vector, 10k ops / 10 peers | 50.125 µs | 21.47 ns | -99.96% |
| State vector, 10k ops / 1k peers | 180.31 µs | 3.616 µs | -98.0% |
| Encode 10k × 32-byte payloads | 174.69 µs | 35.23 µs | -79.8% |
| Encode 10k × 1 KiB payloads | 455.64 µs | 56.01 µs | -87.7% |

The index/cache/`Arc` alternatives all earned their complexity. Incremental RGA ordering and run-length text remain explicitly deferred to PR-15.

**Audit (verified, fanned out over 3 slices):** All claims TRUE. Block index is correct via *two* independent defenses — a generation stamp (fast path) plus a per-lookup identity re-check that rebuilds on mismatch — so even a generation *collision* (whole-`IndexedBlocks` swap, exercised by `block_index_repairs_after_public_sequence_replacement`) cannot return a wrong block; the private `sequence` field forces every mutation through the generation-bumping `DerefMut`. State-vector cache is sound: applied ops are strictly append-only (an incremental max is valid) and all five insertion paths (`apply_op`, `apply_one`, `promote_ready_pending`, `add_local_op`, `restore_applied`) call `observe`. `Arc<[u8]>` change is behavior-preserving (`ptr_eq` sharing test + JSON byte-array wire-shape test; serde `rc` enabled). Benchmarks measure the right operations with no timing-bound assertions (no CI flakiness); no phase language leaked into code. **TDD gap closed:** the state-vector test compared the cache only to explicit expected values and to a restore that reuses the same cache, so a systematic `observe` bug would pass both sides; added `cached_state_vector_matches_independent_recompute` (`tests/performance_indices.rs`) pinning the cache to a from-scratch max-per-peer recompute over `applied_ops()`. **Non-blocking notes:** split/merge index re-lookups are already covered transitively (`merge_blocks_preserves_right_unit_ids_and_converges` asserts the tombstoned right block is unfindable; the split test finds the new suffix block); the committed bench measures only the "after" state (before column requires the standard Criterion baseline checkout). **Out-of-scope pre-existing observation (not PR-14):** `raw_apply_op` mark arms resolve `block_elem_id` tree-wide but then call top-level-only `blocks.get_element`, so marks on *nested* blocks would return `BlockNotFound` — a latent mark limitation, harmless today since marks have no collaborative wire op.

## PR-15 incremental sequence ordering — **done**

- `sequence_incremental` is an empty, default-off Cargo feature; the existing full-rebuild behavior remains the default soak/control path.
- With the feature enabled, insertion finds the next ordered sibling or the end of the anchor subtree, inserts once, and repairs shifted index entries without rebuilding and sorting the full sequence.
- Debug builds compare the completed incremental apply (including a released pending batch) with a cloned full rebuild. The differential generator now varies `right_origin` and compares against the naive oracle after every operation.
- The 10k reverse-causal buffering test and the complete workspace suite pass with the feature enabled. No wire, snapshot, storage, or OpId representation changed.

### Criterion before/after (100 samples, default Criterion timing)

| Benchmark | Full rebuild | Incremental | Change |
| --- | ---: | ---: | ---: |
| Top-level middle insert, 1k elements | 133.28 µs | 5.987 µs | -95.5% |
| Top-level middle insert, 10k elements | 2.074 ms | 96.65 µs | -95.3% |
| Nested text middle insert, 1k units | 144.07 µs | 5.921 µs | -95.9% |
| Nested text middle insert, 10k units | 2.165 ms | 96.69 µs | -95.5% |
| Structural serialize, 1k bytes | 2.102 µs | unchanged path | baseline only |
| Structural serialize, 10k bytes | 17.57 µs | unchanged path | baseline only |

Run-length text was deliberately not implemented. The insertion profile no longer supports paying its storage, split, mark-expansion, and compatibility costs; it remains a future option only if later memory or editing profiles produce new evidence.

**Audit (verified):** `compare_siblings` is a faithful extraction of the prior rebuild comparator (identical right_origin / id tie-break logic), and `insert_incrementally` places a new child at the next-greater sibling's index (after that sibling's whole subtree) or at `subtree_end(after)`, matching the rebuild's tree order. Confirmed empirically: the upgraded differential proptest (now varies `right_origin`, compares against the naive oracle after *every* op) plus `debug_assert_incremental_order` (clones + full-rebuilds + compares element ids on every apply) pass with the feature enabled — 20k differential cases and the full 360-test workspace suite all green under `--features sequence_incremental`. **Gap found + fixed (gate coverage):** `sequence_incremental` is default-off, and neither `just check` (`cargo test --workspace`) nor CI's `just differential-test` enabled it, so the entire incremental algorithm and its debug dual-path check ran in *no* automated gate — a regression in `insert_incrementally`/`subtree_end`/`compare_siblings` would keep the gate green. Fixed by making the `differential-test` recipe run the oracle proptest in **both** configurations (default rebuild + `sequence_incremental`); CI already invokes this recipe in the PR and nightly jobs, so both ordering strategies are now covered with the debug assertion active. No production wire/snapshot/OpId behavior changed (feature is off by default).

## PR-16 storage generation protocol — **done**

- The pre-existing superblock layout is frozen as `SuperblockV1`; a fixed byte fixture proves old metadata and its legacy `segment` payload remain readable.
- V2 metadata adds a monotonic generation and appends a little-endian CRC32 trailer over the archived body. Readers decode both slots independently, validate their paired payload length/checksum, and select the highest valid generation.
- A write replaces only the lower-generation slot: its paired payload is durably published first, then its superblock. The other metadata/payload pair remains untouched as the rollback generation.
- File contents are synced before atomic rename and the containing directory is synced after publication on Unix. Other platforms retain atomic replacement and file sync, but directory durability is not overclaimed.
- Compaction copies active generations into the archive instead of moving them away before the replacement snapshot commits.
- Op-segment checksums remain the plan's optional follow-up; this slice changes snapshot durability only.

**Ablation:** alternating only V2 superblocks over one shared `segment` was rejected. The segment-before-metadata crash test demonstrates the invariant: replacing shared bytes makes the older superblock's checksum unusable, so there is no recoverable previous generation. Pairing one segment with each metadata slot passes the same test and newest-superblock corruption recovery, at the explicit cost of up to two active snapshot payloads.

**Audit (verified):** crash-safety traced sound — writes target the lower-generation slot so the newer slot is always an intact fallback; `atomic_write_durable` fsyncs the temp file, renames, then fsyncs the directory (Unix), and the segment is durable before the superblock (the commit point); both superblock (CRC trailer) and segment (checksum-in-superblock) are integrity-checked; `decode_superblock` tries V2 (CRC + rkyv) then falls back to V1 (generation 0, so V2 always supersedes). Tests cover alternating slots + monotonic generations, corrupt-newest read fallback, crash-after-segment-before-superblock, and the frozen V1 fixture read+upgrade. **TDD gap closed:** `read_slot_generation` swallows a corrupt slot's decode error to `None`, making that slot the next write target (self-repair) — but only the *read-side* fallback was tested. Added `write_after_corruption_repairs_bad_slot_without_clobbering_good_fallback` (`src/storage/mod.rs`): corrupt the newest slot, write again, and assert the write repairs the bad slot while the sole surviving good slot keeps its original generation and the newest read is the repair. **Non-blocking notes:** after a V1→V2 upgrade the legacy `segment` file lingers indefinitely (harmless dead weight, never read once both slots are V2); double-buffering roughly doubles on-disk snapshot bytes (inherent crash-safety cost, and the `active_storage_bytes` overhead assertion was correctly relaxed to match). Op-segment checksums remain a documented follow-up.

## Gate

- `just check` — **passed** after PR-16 audit (365 tests, 0 warnings; added write-side corrupt-slot repair test)
- `just check` — **passed** for PR-16 (364 tests, 0 warnings)
- `cargo llvm-cov --workspace --all-features --summary-only` — **passed**; 90.80% repository line coverage / 89.48% region coverage, above the PR-15 baseline; `src/storage/mod.rs` is 96.10% line-covered
- Storage compatibility/crash ablation — **passed**; frozen V1 bytes upgrade to one V2 slot while retaining V1 fallback, and paired payload slots recover after newest-superblock corruption or segment-before-metadata interruption
- `just check` — **passed** after PR-15 audit (360 tests, 0 warnings; default full-rebuild path)
- `just differential-test` — **passed** after audit fix; now runs the oracle proptest in **both** default and `sequence_incremental` configurations (was default-only)
- `cargo test --workspace --features sequence_incremental` — **passed** (360 tests; debug dual-path assertion active on every apply)
- `cargo test --workspace --all-features` — **passed** (360 tests, 3 manual dhat tests ignored; incremental path and 10k reverse-causal chain green)
- `PROPTEST_CASES=100000 cargo test --test core_differential --features sequence_incremental differential_test_sequence -- --exact` — **passed**; oracle and debug full-rebuild comparison green after every generated operation
- `cargo llvm-cov --workspace --all-features --summary-only` — **passed**; 90.61% repository line coverage / 89.21% region coverage, above the PR-14 baseline, with all new incremental-ordering lines covered
- Criterion before/after probes — **passed**; 10k top-level and nested insertion improved by about 95.4%; serialization baseline recorded; run-length deferred
- `just check` — **passed** after PR-14 audit (359 tests, 0 warnings; added independent state-vector recompute oracle)
- `just check` — **passed** for PR-14 (358 tests, 0 warnings; differential/oracle green)
- `cargo llvm-cov --workspace --all-features --summary-only` — **passed**; 90.52% repository line coverage / 89.04% region coverage, both above the PR-13 baseline, with >90% coverage on PR-14 changed source lines
- `cargo bench --bench performance -- --sample-size 20 --measurement-time 1 --warm-up-time 0.5` — **passed**; all PR-14 target benchmarks improved materially from baseline
- `just check` — **passed** after PR-13 audit (350 tests, 0 warnings)
- `just check` — **passed** for PR-13 (349 tests, 0 warnings)
- `cargo llvm-cov --workspace --all-features --summary-only` — **passed**; 90.36% repository line coverage / 88.94% region coverage, no global regression, with >90% coverage on PR-13 changed source lines
- `just check` — **passed** after PR-12 audit (338 tests, 0 warnings)
- `cargo llvm-cov --workspace --all-features --summary-only` — **passed**; 88.36% repository line coverage (up from 85.93%), with >90% coverage on PR-12 changed lines
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
