# Architecture Evolution — Plan State

Companion to [`architecture-evolution.md`](architecture-evolution.md).

| Field | Value |
| --- | --- |
| **Plan** | `docs/architecture-evolution.md` (Draft revision 15) |
| **Last updated** | 2026-07-16 |
| **Tracking unit** | PR slices under design phases A–Q |
| **Joint consumer plan** | `../md-mcp/docs/joint-md-crdt-v2-implementation-plan.md` |

**Release compatibility policy:** only V3 session snapshots and current V2 dual-slot storage are
accepted. Earlier completed entries record what landed historically; the temporary snapshot V1/V2
and storage V1 readers, upgrade branches, and fixtures are removed. Older vault state must be
reinitialized and re-ingested from Markdown.

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
| **PR-16** Storage V2 generation protocol | **done** | V2 CRC trailer; alternating metadata/payload slots; file + directory sync; crash fallback. The temporary V1 reader was removed in PR-35. |
| **PR-17** Criterion benchmark suite | **done** | Sequence, nested text, public `insert_text`, serialization, block index, state-vector, and change-encoding probes; default + incremental `just bench` |
| **PR-18** Module splits | **done** | Document parser/serializer, session wire translation/application, and sync validation extracted behind unchanged module façades |
| **PR-19** FFI publish decision | **done** | `md-crdt-ffi` remains unpublished and API-empty; placeholder function removed; manifest/README honesty enforced by test |
| **PR-20** CLI/session examples | **done** | Global `--vault` root; accurate command help; high-level peer exchange and multi-document `VaultSession` README workflows |
| **Phase D exchange follow-up** | **done** | Path-scoped state-vector/delta/apply APIs; concurrent external edits converge across two vaults and persist across reopen |
| **PR-21** Joint workspace contract | **done** | Persistent vault/document identity, opaque revisions, direct Rust consumer contract |
| **PR-22** Lossless source model | **done** | Per-root source regions preserve untouched bytes and opaque syntax |
| **PR-23** Stateful ingest/durable export | **done** | Revision-checked refresh and crash-safe single-document Markdown publication |
| **PR-24** Inline marks/links | **done** | Semantic grapheme text + causal mark/link intervals; delimiter attrs, wire, and snapshot history |
| **PR-25** Collaborative frontmatter | **done** | Lossless raw base, per-key LWW ops, comment/order preservation, opaque rejection |
| **PR-26** Block/section move | **done** | Atomic fresh-placement range moves; identity preservation; move/delete and cycle semantics |
| **PR-27** Parsed table ingest | **done** | Table metadata and row insert/update/delete/reorder; table/prose ids preserved |
| **PR-28** Mark-preserving replacement | **done** | Boundary affinity, retained-range projection, whole-replace drop, Unicode anchor helpers |
| **PR-29** Descriptors/change summaries | **done** | Paginated body-free descriptors; before/after affected-id summaries (created/deleted/moved/updated); audit added `deleted`-categorization test |
| **PR-30** Atomic document batches | **done** | Snapshot-probe apply + swap-on-success: all-or-nothing, no clock burn; revision precondition + preview token. Audit added `StaleRevision`/TOCTOU tests |
| **PR-31** File lifecycle/multi-doc transaction | **done** | Create/rename/delete; journalled crash-recoverable multi-doc export. Audit added orphan-`.pending` sweep + test |
| **PR-32** Operation-segment integrity | **done** | Durable magic/version/length/CRC framing; contiguous reads fail closed without advancing snapshot state |
| **PR-33** History compaction/rebase | **done** | Caller-managed acknowledged leases, bounded retained log, delta floor, checkpoint epoch, and full-snapshot rebase |
| **PR-34** Frozen `md-mcp` contract | **repository producer done; joint gate pending** | Versioned public-DTO fixture is committed; sibling consumption and token/task gate are outside this repository slice |
| **PR-35** Final compatibility purge | **done** | Snapshot V1/V2 and storage V1 readers/fixtures/aliases removed; current formats only |
| **PR-36** Stable anchored edits/scoped preconditions | **done** | Stable start/end/unit targets; scoped semantic/placement digests; contract fixture v2; 100/100 unrelated-churn replay; Criterion gate ≤3.45% midpoint overhead; 98.12% changed-line coverage |
| **PR-37** Bounded semantic projections | **done** | Owned field-selective DTOs, hard bounds/continuations, exact owned regions, frozen 3,055-byte transcript; 10k one-block gate ≈1,618× latency, 313× output, and 2,231× allocation improvement |
| **PR-38** Revision-bound hierarchy cursors | **done** | Stateless revision/parent/identity cursor; typed restart failures; node-local digest/count summaries; contract v3; wide/deep ablation gate |
| **PR-39** Cell-addressable tables | **planned** | Depends on PR-36/38 |
| **PR-40** Complete structured mutations | **planned** | Depends on PR-36–39 |

## Phase B checklist

- [x] In-memory paragraph as `Sequence<TextUnit>` (PR-06a)
- [x] Parse + serialize round-trip on units
- [x] insert_text inserts grapheme units (local API)
- [x] Unit-backed V3 snapshot format landed; temporary legacy readers were removed in PR-35
- [x] Wire InsertText/DeleteText + session commits (PR-06b)
- [x] Concurrent multi-peer paragraph edits (PR-06b)

## Decisions log (implementation)

| Decision | Choice | Rationale |
| --- | --- | --- |
| PR-05 before 06a? | **Skip optional PR-05** | MVP gate is 01–04 + 06a/b + 07 |
| Unit OpIds on skeleton expand | `parent_elem.counter + 1 + i` same peer | Deterministic across peers; wire still only carries block id + string |
| Snapshot paragraph body | Current V3 snapshot only; V1/V2 readers removed | The joint pre-1.0 release has no migration obligation; one accepted representation reduces branches and synthetic-id risk |
| Span-aware sync (audit) | `Operation` covers a counter range `[e-span+1, e]`; `Operation.id` = max embedded id (N1); readiness gates on range start vs frontier | Lets one op reserve a contiguous id range (block + G units) without sparse op ids; span 1 = old behavior; unblocks multi-unit InsertText |
| Unit-mode default | `CollaborativeDocument::new` → `unit_mode = true` | Phase B cutover; string-mode still available via `with_codec(..., false)` |
| N6-d insert_paragraph | empty `InsertBlock` then `InsertText` (two commits) | Pure N1–N4; no skeleton range-seed |
| Nested text apply | `Sequence::with_value_mut` on block element | Avoid full `Block` clone per unit |
| Mark unification | Keep rich causal `core::mark::MarkSet`; delete generic `MarkSet<K,V>` / `TextAnchor` | Single public API; matches `render_spans` + text-unit anchors |
| Deprecated aliases | `RichMarkSet` / `RichMarkInterval` removed | The joint pre-1.0 release does not retain an unused parallel naming surface |
| Session snapshot dir | `.mdcrdt/sessions/<rel>.mdcrdt` (not `.mdcrdt/state/`) | Avoid collision with existing fingerprint `LastFlushedState` blobs in `state/` until ingest unifies storage |
| Peer id format | Decimal `u64` text in `.mdcrdt/peer_id`; non-zero | Shared clock domain for all file sessions in a vault |
| Ingest D1 scope | Structure only (add/remove); matched different text deferred to D2 | Ship block ops without grapheme LCS |
| Ingest new paragraphs | `insert_paragraph` (N6-d) | Empty InsertBlock + InsertText body |
| Pure reorder | `MoveBlocks` allocates fresh placements while preserving logical ids | Re-ingest and explicit moves now update CRDT order instead of only retaining match ids |
| Nested machinery packaging | Landed in same commit as PR-09 + plan-state | Prefer: feature commit (code+tests) then plan/docs commit, or a dedicated PR slice |
| Quote editing via vault | Structure re-ingest supported; in-quote *text* LCS still PR-10 | Nested structure add/remove/re-ingest preserve quote + matched child ids |
| Structure match floor | `min_match_score: 5000` for re-ingest | Default 2000 allowed content_sim=0 matches via position alone |
| Text ingest LCS | Visible graphemes; deletes high→low then inserts; batch insert runs | Preserves unit OpIds on LCS and Phase J projects mark ranges through retained semantic endpoints |
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
| Benchmark configurations | Run the default rebuild control and `sequence_incremental` treatment | An all-features-only run loses the control measurement; two explicit invocations make performance regressions and feature value comparable |
| Public text benchmark setup | Restore a prepared session snapshot outside the timed interval, then time one middle `insert_text` | Measures block lookup, unit construction, wire encoding, document apply, and sync-log integration without charging fixture construction |
| Module split boundaries | Split by responsibility, not equal line counts | Parser/serializer, wire translation/application, and validation are cohesive seams with narrow parent access; arbitrary chunks would reduce file size while increasing cross-module coupling |
| FFI release surface | Keep `md-crdt-ffi` unpublished and expose no placeholder API | A real C ABI needs explicit ownership, allocation, UTF-8, error, panic, and header contracts. No target language or consumer is defined, so publishing a token surface would create unsupported compatibility debt |
| CLI vault selection | One global `--vault <PATH>` with `.` as the default | Makes every existing command usable from outside the vault without duplicating path arguments or inventing new transport/export commands |
| Vault exchange surface | Path-scoped `state_vector`, `encode_changes_since`, and `apply_remote`; persist after successful remote apply | Keeps transport outside the library, preserves per-document isolation, and prevents a successful vault-level apply from remaining memory-only |
| Release engine | Concrete `VaultSession` workspace only; no public generic engine abstraction | Prevent a permanent second parser/model/editor/serializer in `md-mcp` |
| Lossless model | Semantic nodes with attached source spans/trivia and opaque unsupported regions | Exact scoped edits without maintaining a second mutable document tree |
| Persistent identity | Content-independent `VaultId`/`DocumentId`; opaque `RevisionToken` | Handles survive edits while stale mutation preconditions remain precise |
| Export ownership | `VaultSession` separately names snapshot save and durable Markdown export | A successful state save must not imply that the user's Markdown was published |
| Inline authority | Unit-anchored marks/links plus source trivia | Avoid divergent raw-delimiter and causal-mark representations |
| Move identity | Preserve block, descendant, unit, row, and mark ids; section moves are atomic | Agent targets remain valid and peers never observe a half-moved section |
| Table ingest boundary | Emit table and row/metadata operations; never skip the containing file | Prose in a table-bearing file remains discoverable/editable |
| Replacement mark policy | Split/trim/project anchored intervals deterministically; never silently broaden | Preserve justified formatting without inventing marked content |
| Move placement ablation | Atomic range envelope over permanent per-id placement register | Sequence remains the single ordering authority; fresh placements preserve payload identities and make sections indivisible |
| Concurrent move winner | Highest fresh placement id wins; losing placements materialize as tombstones | All replicas retain the same sequence history independent of delivery order |
| Move/delete race | Logical-id delete wins | A delete targets the current winning placement, so delivery order cannot resurrect a concurrently deleted block/row |
| Section membership | Capture the contiguous heading range at commit time | Concurrently inserted section children stay where inserted; the move payload is finite and deterministic |
| Mark insertion affinity | Outside at start/end boundaries; expand strictly inside | Prevents boundary broadening while matching editor expectations inside formatted text |
| Frontmatter fallback | Lossless raw base + per-key LWW; unsupported YAML is opaque | Structured edits preserve comments/order/quotes and fail closed rather than canonicalizing complex YAML |
| `md-mcp` boundary | Core owns identity/semantics/deltas/batches; MCP owns schemas/search/cursors/token budgets | Correctness stays at the state owner and presentation policy at the protocol boundary |
| Multi-document publication | Intent journal with idempotent recovery | Cross-file edits cannot report success with partially published Markdown |
| Storage compatibility | Current V2 dual-slot format only; V1 reader/fixture and upgrade branches removed | Two V2 generations provide crash recovery without preserving an unpublished legacy format |
| Operation-segment frame | Magic + version + payload length + CRC32, published with the durable atomic-write helper | A torn append fails independently and cannot alter the prior snapshot generation or readable log |
| Peer retention lease | Caller includes every currently supported peer and its acknowledged frontier on each checkpoint; omission expires it | Deterministic inputs replace nondeterministic wall-clock expiry and keep policy at the host boundary |
| Tombstone checkpoint policy | `KeepAll` only | Peer operation acknowledgement is insufficient proof that structural causal references are collectible |
| Lagging-peer recovery | Per-origin delta floor plus full V3 `SessionSnapshot` rebase | Bounds retained history without silently abandoning a supported peer |
| Joint contract fixture | Versioned serialized public-DTO producer fixture in this repository | Freezes a concrete cross-repo shape without introducing MCP response types into the core |
| Projection representation ablation | Owned typed DTOs; exact source is opt-in; borrowed visitor remains benchmark-only | Visitor traversal is faster but cannot provide serde ownership, hard byte accounting, continuation, or a cross-language contract |
| Projection lookup | One early-exit traversal retaining only requested identities | Avoids cold construction of the document-sized block index while preserving ordered selected output |
| Descriptor continuation | Stateless revision/parent/traversal/last-id cursor with validated physical hint | Fails closed on mutation without retained server state and advances in O(page size), including page-size-1 scans |
| Descriptor digest | Node-local semantic digest plus hierarchy counts and `ChangeSummary`; subtree digest omitted | The recursive control was only about 2.5× slower than the node-local lower-cost control, so a cache did not demonstrate the required 5× threshold and would add invalidation risk |
| Semantic mark digest | Sort/deduplicate active semantic marks and exclude the `delimiter` attribute | CRDT interval ids/order and source delimiter choice are presentation/history, not semantic content |
| Lossless representation ablation | Per-root semantic block source regions with owned leading trivia | Smallest option that preserves unsupported bytes and localizes edits. A piece table still needs semantic ownership mapping; a compact CST duplicates the authoritative CRDT tree and increases synchronization risk. |
| Workspace identity files | UUID text in `.mdcrdt/vault_id` and path-scoped `.mdcrdt/document_ids/` entries, published durably | Content-independent handles survive edits/reopen; invalid identity bytes fail closed instead of silently replacing identity. |
| Revision representation | Opaque 128-bit digest of the session snapshot | Detects observable state changes without exposing log or hashing details as API. |
| Markdown publication scope | Durable `export_markdown` for one document; no loop-shaped `export_all_markdown` | A loop can partially publish and imply false atomicity. Cross-document publication waits for PR-31's intent journal and recovery contract. |

## Phase C checklist

- [x] One public `MarkSet` (rich causal)
- [x] `Block.marks` / `EditOp` use rich types
- [x] Generic LWW mark types removed from `core`
- [x] `render_spans` over paragraph unit order (`Document::render_paragraph_spans`)
- [x] Expand-on-insert / range-remove helpers still green

## Phase D checklist — complete

- [x] Multi-file vault opens distinct sessions per path; shared peer id (PR-08)
- [x] Open/save per-file session snapshots (PR-08)
- [x] Structure ingest: hash gate → match → Insert/Delete block → snapshot (PR-09)
- [x] Block reorder preserves ids via matching; Phase J now emits native CRDT moves
- [x] Blockquotes preserved on **first** ingest (not flattened); Phase J removed the historical table skip
- [x] External in-paragraph text edit → InsertText/DeleteText (PR-10)
- [x] **Nested re-ingest matching** (structure + in-quote text LCS via PR-10)
- [x] Two vaults exchange ops after external edits

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

## Phase G checklist — historical implementation; legacy reader removed by PR-35

- [x] Dedicated V1 decoder exercised by frozen pre-V2 bytes
- [x] V2 body uses generation metadata plus a little-endian CRC32 trailer
- [x] Writes alternate a single metadata/payload slot and preserve the previous generation
- [x] Payload and superblock temp files are synced before rename; containing directory is synced on Unix
- [x] Missing/corrupt newest metadata and interrupted payload publication fall back to the prior valid generation

## Phase H checklist — complete

- [x] Criterion suite exists under `benches/` and is registered in Cargo
- [x] Sequence middle insert and nested `Sequence<TextUnit>` insert at 1k/10k
- [x] Public `CollaborativeDocument::insert_text` at 1k/10k
- [x] Structural serialization at 1k/10k
- [x] `just bench` runs default and incremental ordering configurations
- [x] Module splits after churn settles (PR-18)
- [x] FFI implementation or explicit unpublish decision (PR-19: unpublish)
- [x] CLI and README multi-document workflow polish (PR-20)

## Phase I checklist — complete

- [x] PR-21: freeze persistent identity, opaque revision, descriptor/summary, batch/receipt, and export contracts
- [x] PR-21: local direct-consumer compile fixture freezes the concrete API without a public generic engine trait
- [x] PR-22: unchanged open/export is byte-identical, including opaque unsupported syntax
- [x] PR-22: one-word edit changes only its owned source region
- [x] PR-23: refresh/ingest enforce expected revision or fingerprint without mutation on rejection
- [x] PR-23: single-document Markdown export is durable and separately named from snapshot save

The companion `md-mcp` cutover fixture remains owned by its repository and is part of the joint
PR-34 release gate. This repository's Phase I fixture is `tests/workspace_contract.rs`; no sibling
repository was modified during this slice.

## Phase J checklist — complete

- [x] PR-24: parse/serialize marks and links through one causal unit-anchored model
- [x] PR-24: snapshot and exchange preserve mark/link history and delimiter trivia
- [x] PR-25: frontmatter field edits preserve comments/order/unsupported YAML through opaque fallback
- [x] PR-26: top-level/nested/section moves preserve all logical identities and converge
- [x] PR-27: table-bearing files ingest structurally instead of returning `Skipped`
- [x] PR-28: external replacement follows tested mark-boundary affinity and Unicode mapping rules

## Phase K checklist

- [x] PR-29: body-free descriptors avoid cloning or serializing all blocks
- [x] PR-29: local/remote/ingest/export summaries are bounded by affected ids
- [x] PR-30: expected-revision batches are atomic and burn no ids on rejection
- [x] PR-30: compact receipts return affected ids/post-revision; full diff stays opt-in
- [x] PR-31: file create/rename/delete and cross-document batches recover from every crash point
- [ ] `../md-mcp`: direct concrete workspace powers context → preview → apply → scoped verify (joint consumer; PR-34)

## Phase L checklist — repository work complete; joint release gate pending

- [x] PR-32: operation segments reject corruption/truncation and preserve the prior readable state
- [x] PR-33: checkpoint/compaction bounds history and deterministically rebases lagging peers
- [x] PR-34: versioned `md-crdt` producer fixture freezes the concrete public DTO surface
- [ ] PR-34: pinned `md-mcp` consumer suite passes; temporary adapters and duplicate engines are deleted
- [x] PR-35: delete session-snapshot V1/V2 and storage V1 readers, constants, upgrade branches, and fixtures
- [x] PR-35: reject every non-current persisted format with an explicit reinitialize/re-ingest error
- [x] PR-35: remove remaining deprecated compatibility surfaces in this repository
- [ ] Joint token/task-success and lossless/crash-recovery release gates pass again after cleanup

## Phase L implementation outcome

- Operation segments are independently framed by magic, format version, payload length, and CRC32.
  Append uses the existing durable atomic-write boundary; sorted reads require contiguous segment
  indices and fail closed on header, length, checksum, truncation, or gap errors.
- Checkpoints prune only the number of oldest operations needed to satisfy
  `max_retained_ops`, and only when every active caller-supplied lease acknowledges them. A blocked
  checkpoint is non-mutating. Successful checkpoints advance an epoch and per-origin delta floor,
  persist both in V3 snapshots, and return a full checkpoint through `sync_since` when a peer is
  below the floor. After rebase, incremental deltas resume normally.
- Caller-managed acknowledgement leases were selected over wall-clock expiry because the latter
  makes identical checkpoint requests depend on runtime timing. Tombstone GC remains `KeepAll`:
  acknowledgements do not prove that retained structural references are unreachable.
- The producer fixture at `tests/fixtures/workspace-contract-v3.json` is generated from actual
  public DTO serialization and covers handles, descriptors, summaries, edit batches, receipts,
  export/recovery, checkpoint requests, and rebase errors. The sibling consumer gate was not run
  because this slice is restricted to `md-crdt`.
- V3 is the only accepted session snapshot and current V2 dual-slot storage is the only accepted
  superblock. Non-current versions and detected legacy storage artifacts return an actionable
  reinitialize/re-ingest error. Synthetic legacy conversion and deprecated rich-mark aliases are
  removed.
- Criterion control/treatment: encoding 10,000 retained operations was 40.588–41.815 µs versus
  0.841–0.897 µs with 128 retained (about 47× faster); restoring 1,002 operations was
  118.54–122.36 µs versus 51.33–53.11 µs with 64 retained (about 2.3× faster).

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

## PR-17 Criterion benchmark suite — **done**

- `benches/performance.rs` covers sequence middle insertion, nested text-unit insertion, public session `insert_text`, structural serialization, BlockId lookup, state-vector generation, and delta encoding.
- The public text probe prepares a real `CollaborativeDocument`, restores its snapshot outside the timed interval for each sample, and times one middle-grapheme insertion through validation, wire encoding, document apply, and sync logging.
- `Cargo.toml` registers the Criterion target and `just bench` executes both the default rebuild control and `sequence_incremental` treatment.

### Public insert_text ablation (20 samples, 1 s measurement)

| Benchmark | Full rebuild | Incremental | Change |
| --- | ---: | ---: | ---: |
| Session middle insert, 1k graphemes | 143.68 µs | 11.34 µs | -92.1% |
| Session middle insert, 10k graphemes | 2.216 ms | 155.6 µs | -92.7% |

The session result retains the same scaling signal as the lower-level sequence probe while exposing the fixed validation/codec/log overhead. Keeping both probes is justified: the sequence benchmark attributes ordering cost; the session benchmark represents the caller-visible keystroke path.

**Audit (verified):** all seven probes present and wired into `criterion_group!`; the new `session_insert_text` correctly restores the snapshot *outside* the timed `Instant` window so only the insertion is measured. Beyond compile-checking (clippy `--all-targets --all-features`), both configurations were **executed** to confirm no runtime panic or ordering divergence: `cargo bench --bench performance` and `cargo bench --bench performance --features sequence_incremental` each ran all 14 parameterized cases to completion (EXIT 0). No phase language in bench code; `architecture-evolution.md` Phase H item 1 correctly flipped to done. No defects found — benchmark-only slice, no production behavior changed.

## PR-18 module splits — **done**

- `doc` delegates parsing to `parser.rs` and structural rendering/helpers to `serialize.rs`; `Parser` remains re-exported from `md_crdt::doc`.
- `session` delegates envelope extent checks, peer validation, block skeleton conversion, and document application to private `wire.rs` helpers.
- `sync` delegates validation errors, limits, and `validate_changes` to `validation.rs` while preserving every root-level public re-export.
- A module-boundary regression test pins the four responsibility seams. Behavioral tests cover serializer edge cases, nested wire shape conversion, defensive span handling, validation messages, and table metadata updates.

**Ablation:** equal-sized chunks and façade-only file renames were rejected. They reduce visible line counts without creating ownership boundaries. Responsibility-based extraction moves cohesive algorithms intact, requires only `pub(super)` for session helpers, and leaves all public paths and data formats unchanged.

**Audit (verified, fanned out over the three splits + a public-API-surface diff):** all extractions are faithful, behavior-preserving moves. Sync/validation: byte-identical, and the security-relevant default limits are unchanged (`max_ops_per_message=10_000`, `max_payload_bytes=10 MiB`, `max_pending_buffer=100_000`). Session/wire: byte-identical (one rustfmt reflow after adding `pub(super)`); convergence-critical `operation_extent` span math, `max_counter_in_kind`, split/merge/table-row apply arms, and `check_peer_consistency` all preserved; helpers are `pub(super)`, not `pub`. Doc/parser+serialize: `mod.rs` gains only `mod`/`use` plumbing plus one additive test (no production logic added), and moved bodies are byte-identical (spot-checked `parse_table_delimiter`). A diff of every `pub` symbol before/after confirms **no public item was dropped and none was newly exposed** beyond the `pub use` re-export plumbing. **Test quality fixed:** the original `module_boundaries.rs` only `include_str!`-grepped parent modules for `mod X;` substrings — it would pass even if a `pub use` re-export were deleted, and matched commented-out lines. Replaced it with a compile-level façade + delegation guard (`tests/module_boundaries.rs`): it references each split's public path (`Parser`, `EquivalenceMode`, `CollaborativeDocument`, `ValidationLimits`/`ValidationError`/`MalformedKind`/`validate_changes`), round-trips parse/serialize, applies a remote op through the wire helpers, and asserts the default validation limits — so a broken re-export or a loosened limit now fails the build/gate.

## PR-19 FFI publish decision — **done**

- `md-crdt-ffi` remains a workspace member with `publish = false`; its manifest description now identifies it as an unpublished placeholder and no longer advertises C-API metadata.
- The template `add` function and test were removed. The crate-level documentation explicitly says there is no runtime or foreign-function API.
- README and contributor-facing workspace descriptions state that no C ABI or supported language binding exists and direct Rust consumers to `md-crdt`.
- `packaging_contract.rs` prevents accidental publication claims, native library artifact declarations, restoration of the fake API, or loss of README honesty.

**Ablation:** implementing a nominal C API was rejected. Even a thin `CollaborativeDocument` wrapper must define handle ownership/destruction, allocator ownership for returned bytes, UTF-8 and null handling, stable error codes/messages, panic containment, thread-safety, and header generation. With no target language or consumer requirements, those choices would be speculative public ABI commitments. An explicit unpublished, API-empty placeholder is the smaller honest surface.

**Audit (verified):** `md-crdt-ffi/Cargo.toml` has `publish = false`, an honest description, empty `[dependencies]`, and no `[lib] crate-type` (so no `cdylib`/`staticlib` native artifact); `src/lib.rs` is documentation-only with the placeholder `add` removed; README and CONTRIBUTING match the honesty strings the test pins. **Test-strength gap closed:** the contract only asserted `!source.contains("pub fn add")` — the *specific* old placeholder name — so a future `pub extern "C" fn …` or `#[no_mangle]` would slip past the very test meant to keep the crate API-empty. Generalized `packaging_contract.rs` to forbid any exported/foreign surface (`pub fn`/`pub struct`/`pub enum`/`pub extern`/`extern "C"`/`#[no_mangle]`/`#[unsafe(no_mangle)]`), so the "API-empty" claim is now actually enforced.

## PR-20 CLI and multi-document workflows — **done**

- Every command accepts a global `--vault <PATH>` and retains the current directory as the default.
- Command help identifies `ingest`/`sync` as all-file, per-session ingest and describes `status`/`flush` as fingerprint tracking.
- README peer exchange uses `CollaborativeDocument::{encode_changes_since,apply_remote}` instead of raw opaque `SyncState` payloads.
- README multi-document usage uses `VaultSession`, showing one stable peer shared by independent path-keyed documents and snapshots.
- The CLI documentation states the actual boundary: `sync` emits local operations but does not transport them, and `flush` records fingerprints rather than exporting CRDT session state.

**Ablation:** a docs-only wording pass was rejected because it left callers tied to changing the process working directory. New exchange/export subcommands were also rejected because the plan defines no transport, remote endpoint, authentication, or snapshot-to-Markdown export contract. A single global vault-root option improves every existing command without expanding protocol or persistence scope.

**TDD:** `help_describes_vault_root_and_multi_document_commands` failed first because help exposed neither `--vault` nor all-file semantics. `vault_option_ingests_multiple_documents_outside_current_directory` failed first because Clap rejected the option; it now proves two vault-relative files create independent session snapshot paths when invoked from outside the vault.

**Audit (verified + fixed, README honesty):** the five README Rust workflow blocks were written in doctest form but the crate never compiled them, so they were unverified and could rot silently. Wired README into the doctest harness via `#[cfg(doctest)] #[doc = include_str!("../README.md")] pub struct ReadmeDoctests;` (`src/lib.rs`) — the idiomatic pattern that compiles every README ```rust``` block under `cargo test --doc` without duplicating the README onto the rustdoc landing page. The two `VaultSession::open("./notes")` blocks are marked `rust,no_run` (compile-checked, not executed, since they touch the filesystem). Doing so immediately caught a **pre-existing broken example**: the Quick-Start block bound `let first_block = doc.blocks_in_order().first().unwrap();` — E0716, because `blocks_in_order()` returns an owned `Vec` whose temporary is dropped while borrowed — plus an unused `Document` import. Fixed to `let block_id = doc.blocks_in_order().first().unwrap().id;` (`BlockId: Copy`) and trimmed the import. All 6 doctests now pass under `-Dwarnings` (CI mirror), so a future README/API drift fails the gate.

## Phase D cross-vault exchange follow-up — **done**

- `VaultSession::state_vector` and `encode_changes_since` expose one vault-relative document without leaking access to the internal path map.
- `VaultSession::apply_remote` delegates validation and causal integration to `CollaborativeDocument`, then persists the affected session snapshot.
- The two-vault integration test establishes one shared base, ingests distinct external edits at different peers, verifies both deltas are non-empty, exchanges them bidirectionally, and proves state-vector/document convergence survives reopening both vaults.

**Ablation:** direct caller choreography through `session_mut` could already reach the lower-level session primitives, but it made persistence optional and exposed the internal session boundary for a routine vault operation. A vault-wide batch exchange or transport trait was rejected because routing, authentication, retries, and remote discovery are outside the plan. Three path-scoped wrappers are the smallest API that satisfies the per-document exchange contract.

**TDD:** `two_vaults_exchange_external_edits_and_persist_convergence` failed first at compile time because `VaultSession` exposed none of the path-level state-vector, delta, or remote-apply methods. It now covers all new production lines and the persistence boundary.

**Audit (verified):** the three path-scoped methods are thin, correct delegations over the already-audited `CollaborativeDocument` sync; `apply_remote` persists via `save_state`, and Markdown ingest also persists (`save_state` + `write_last_flushed`), so the "persist across reopen" property holds for both the ingest and exchange paths — not only the exchanged case the test drives. The test correctly asserts *convergence* (equal state vectors + serialized text) rather than a specific interleaving of the two concurrent external edits, which is the right CRDT contract.

## Audit — PR-21 / PR-22 / PR-23 (fanned out over three verifiers + API-surface diff)

**PR-21 workspace contract — PARTIAL, hardened.** Identity persistence is real (`VaultId`/`DocumentId` are `Uuid` newtypes persisted to `.mdcrdt/vault_id` and `.mdcrdt/document_ids/`, atomic write, survive reopen — `workspace_contract.rs`). The wired half (identity + revision + disk-fingerprint + handle + export) is genuinely implemented and precondition-checked. Findings: (a) **fixed** — `RevisionToken` derived `Ord`/`PartialOrd` despite being a content digest (never used as a key or sorted); removed the derives before the joint consumer can write meaningless `new > old` comparisons. (b) **Documented, not a defect** — `EditBatch`/`BatchReceipt`/`ChangeSummary`/`BlockDescriptor`/`BlockDescriptorKind` are defined-but-unwired: this is the *deliberate contract freeze* the plan schedules for PR-29/PR-30, not accidental dead code. (c) **Recommendation (not changed — frozen external-contract shape)**: `DiskFingerprint(pub u64)` exposes its raw hash; consider sealing it behind an accessor to match `RevisionToken`'s opaque pattern before md-mcp consumes it.

**PR-22 lossless source — TRUE.** Losslessness is genuinely correct, not just the no-op case: `DocumentSource` slices untouched roots out of an *immutable* `original` string and re-serializes only `dirty` roots, so an edit provably cannot corrupt another region's bytes; `adopt_source_from` drops the source entirely (→ canonical serialization) on any block count/kind divergence, a safety net against stale bytes. Independent checks confirmed multi-region, unicode, and nested-containing-root cases hold byte-exact. **Coverage gap closed:** added `untouched_blockquote_root_with_nested_children_stays_byte_exact` and `multi_region_edit_leaves_untouched_unicode_region_byte_identical` (`doc_lossless_source.rs`) — the latter guards multibyte slice-boundary safety. (Note: nested-child *edit* losslessness is a session-level path — `Document::insert_text` is top-level-only — verified manually by the auditor; a session-level test remains a future coverage item. Latent rough edges, non-blocking: top-level reorder trivia isn't faithfully reproduced; `blocks_mut()` drops the whole source.)

**PR-23 stateful ingest/durable export — TRUE.** Export is genuinely crash-safe (`atomic_write_markdown` = temp + `sync_all` + rename + dir fsync, mirroring the storage layer) and precondition-guarded: `verify_revision`/`verify_disk` run *before* any filesystem mutation, returning `StaleRevision`/`StaleDisk` with the target file untouched (well-tested). Findings: (a) **fixed** — `PublishControl.fail_before_rename` fault-injection was compiled into the production write path; gated it behind `#[cfg(test)]`. (b) **TDD gaps closed** — added `re_export_of_unchanged_document_does_not_rewrite` (idempotence; also pins revision stability across a no-op export) and `export_publishes_into_a_nested_subdirectory` (subdir temp/rename/dir-fsync path). Minor: `sync_directory` is duplicated between `filesync/session.rs` and `storage/mod.rs` (DRY nit, left as-is).

**Plan consistency:** fixed a version drift — plan-state referenced the design doc as revision 8 while `architecture-evolution.md` is revision 9.

## Audit — PR-29 / PR-30 / PR-31 (Phase K, fanned out over three verifiers + independent mechanism checks)

All three verified **TRUE** with no functional bug in the atomicity/recovery cores; the gaps were proof (tests) and two minor real issues, both fixed. Plan-state progress rows were marked done and the Phase K checklist completed (they were still "planned" while implemented — a plan↔code drift, and the design-doc revision reference lagged 10 vs 11).

**PR-29 descriptors/change summaries — TRUE.** The original bounded direct-child descriptor and summary mechanism was genuine; Phase O later replaced its numeric offset surface with revision-bound cursors and replaced recursive descriptor content digests with node-local semantic digests plus hierarchy counts. Summaries remain a real before/after structural diff hardened with an explicit-move-op override and an LIS false-positive guard. **Fixed:** `source_bytes` is pinned to the on-disk parse and goes stale after in-memory edits. **TDD gap closed:** added `local_edit_summary_reports_deleted_block_ids` (the `deleted` category had zero coverage). Residual (documented): the LIS reorder detector is only reachable via a non-move reorder path that isn't deterministically constructible through the public API.

**PR-30 atomic document batches — TRUE (atomic in memory).** Batches apply to a snapshot-restored *probe* and swap into the live doc only on full success, so a mid-batch failure is inherently a no-op — no partial mutation, no Lamport-clock burn (probe drops on rejection; `preview`/`apply_previewed` re-verify the revision, so the token can't let a stale apply through). **Documented:** multi-doc `apply_edit_batches` persistence is in-memory-atomic but **not crash-atomic** (no journal, unlike the export path) — added a doc comment steering durability-sensitive callers to `export_markdown_transaction`. **TDD gaps closed:** added `batch_with_a_stale_expected_revision_is_rejected_without_mutation_or_clock_burn` (the headline precondition had no direct test) and `previewed_batch_rejects_apply_after_an_intervening_edit` (TOCTOU guard).

**PR-31 file lifecycle/multi-doc transaction — TRUE (crash-recoverable all-or-nothing).** A real write-ahead journal is fsync'd *before* any target file is modified, with idempotent redo replay on `open()`; rename preserves `DocumentId` + CRDT history, delete retires identity, and preconditions are prevalidated for the whole batch (one stale request → zero writes). **Fixed (real durability-litter bug):** a crash *after* content pendings were fsynced but *before* the journal left orphan `.pending`/`.backup` temps that recovery (which only scanned `*.json`) never cleaned. Now `recover_pending_transactions` sweeps orphan transaction temps (strict `.<name>.<uuid>.pending|.backup` match), and `export_markdown_transaction` creates the transactions dir *before* writing pendings so the sweep also fires for a first-transaction crash. Added `recovery_sweeps_orphan_transaction_pendings_left_without_a_journal` (with negative cases proving unrelated dotfiles survive). Residual (documented): transaction crash-recovery tests replay fabricated journals — no crash-injection hook on the transaction install path; `RecoveryReport` counts for the real cases remain unasserted.

## Audit — PR-32 / PR-33 / PR-34 / PR-35 (Phase L, fanned out over three verifiers + independent checks)

All verified with no functional bug; the critical CRDT-safety property (PR-33) holds. Gaps were test coverage. (Plan-doc revision was consistent this round.)

**PR-33 history compaction/rebase — TRUE (the critical slice; compaction is provably safe).** An op is pruned only when **every** active lease has acknowledged its counter (`.all(...)` — the minimum-acknowledged-across-all-leases, never the max); pruning removes a contiguous per-peer prefix and `delta_floor[peer]` records every pruned peer's max pruned counter. Any peer whose `since` falls below the floor is unconditionally routed to a full-snapshot rebase (`SyncResponse::Rebase`) that restores the whole document + true frontier and resumes incremental deltas — no path to a silent partial delta or a floor advancing past an unacknowledged op; blocked checkpoints are non-mutating. **Critical TDD gap closed:** every prior test used 0 or 1 lease, so the min-across-leases semantics (the heart of the safety guarantee) was untested — a `.any()`/max regression would not have been caught. Added `checkpoint_uses_the_minimum_acknowledgement_across_multiple_leases` (two leases at ack 4 and 2 → prune capped at 2, `RetentionBlocked { required: 4, eligible: 2 }`, needed ops 3/4/5 survive) and `checkpoint_rejects_duplicate_peer_leases`. **Accuracy notes (documented, not bugs):** `checkpoint_epoch` is currently **inert** — monotonic metadata carried in reports/rebase/snapshots but never compared to detect stale sync; `delta_floor` is the real and sufficient guard. `max_retained_ops` is **advisory** — retention correctly overrides it (a lease-blocked checkpoint leaves the log above the bound rather than dropping needed ops). `DocumentTombstonePolicy::KeepAll` is the only (conservative) policy.

**PR-32 operation-segment integrity — TRUE (fail-closed).** `encode_op_segment` frames magic(8)+version(2)+length(8)+CRC(4)+payload; decode validates all four (header floor, magic, version, declared-length==actual, CRC over the payload slice), so truncation / bad magic / bad version / length / CRC all return `CorruptOperationSegment`. A corrupt or missing **middle** segment fails the whole read (no silent op drop); ordering is numeric (`parse::<usize>` + numeric sort), not lexicographic; appends are atomic+durable. **TDD gaps closed:** added `operation_segment_bad_magic_version_and_short_header_fail_closed` and `operation_segments_read_in_numeric_order_and_reject_a_middle_gap` (bad magic, bad version, header-floor truncation, ≥10 numeric ordering, and the non-contiguous middle-gap path were all untested). **Noted:** the op-segment machinery is complete + tested but **not yet wired into any recovery path** (no production caller) — forward-prep, unit-proven only.

**PR-34 frozen `md-mcp` contract (producer side) — TRUE.** `workspace_fixture.rs` builds live instances of every public workspace DTO (handle, descriptors, `EditBatch` with a rich `WorkspaceEdit` mix, receipts, `CheckpointRequest`/`PeerLease`/`RebaseRequired`) and asserts byte-equality against the committed current fixture (`fixtures/workspace-contract-v3.json`, superseding the pre-release v1/v2 shapes) — a genuine field-by-field regression guard that fails on any renamed/removed/retyped public field. Sibling `md-mcp` consumption + token/task gate remain correctly out-of-repo (PR-34 "joint gate pending").

**PR-35 final compatibility purge — TRUE (fail-closed, complete).** `SNAPSHOT_FORMAT_VERSION = 3`; both snapshot loaders reject any non-V3 with a clear `ReinitializeRequired { found, expected }` (no panic, no silent-empty), and storage returns `ReinitializeRequired` when `LEGACY_SEGMENT_FILE` exists. Grep confirms no leftover dead references (`SuperblockV1`/`ArchivedSuperblockV1`/`SNAPSHOT_FORMAT_VERSION_V1`/`RichMarkSet`/`RichMarkInterval` all removed). Old on-disk state fails closed with a reinitialize-and-re-ingest error rather than corrupting.

## PR-38 revision-bound hierarchy cursors — **done**

- Public numeric offsets are removed. `DescriptorCursor` binds the document, revision, parent,
  traversal, last logical id, and a checksum-protected O(1) physical continuation hint;
  `DescriptorPage` returns the same scope plus `next_cursor`.
- Typed failures cover zero limits, missing parents, corrupt encodings, wrong document/revision/
  parent/traversal, and unresolved cursor anchors. Tests prove changing limits is safe, unchanged
  traversal has no gaps/duplicates, and insert/delete/move require a fresh traversal whose result
  matches the authoritative outline.
- `BlockDescriptor` now reports direct-child/descendant counts and a node-local semantic digest.
  `subtree_digest` is explicitly absent. Presentation-only mark delimiters and causal interval
  ordering are excluded, while recursive projection and scoped-precondition digests remain intact.
- Cursor ablation selected the stateless restart-on-change design: offset paging mixes revisions,
  server snapshots add lifecycle state, and mutation-tolerant resume can obscure reordered or
  inserted siblings. The cursor is fixed-shape (≤256 bytes in memory; 200 bytes in the JSON
  encoding) independently of document size.
- Digest ablation selected node-local summaries. The 64-level recursive end-to-end control measured
  2.8430–2.8982 µs versus 1.1362–1.1452 µs for node-local descriptors. A Merkle cache therefore did
  not demonstrate the required 5× repeated-read threshold and would add ancestor invalidation to
  every mutation path; no cache or optional runtime mode ships.
- The optimized 10,000-wide scan measured 27.309–27.958 ms (limit 1), 37.580–37.911 ms (32), and
  36.089–36.437 ms (256). A 32-item page allocated 19,562 bytes and serialized to 8,302 bytes.
  During audit, removing mark-order materialization for unmarked text and allocation-heavy cursor
  checksumming cut allocation 96.4% and page-size-1 scan latency 66.2% from the first correct build.
  One leaf update plus a root read measured 1.5160–1.5225 s. Node-local summaries skip zero
  descriptors by subtree digest because no subtree cache ships.
- Breaking fixtures are current-only: `workspace-contract-v3.json` and
  `workspace-projection-transcript-v2.json`; the replaced pre-release fixtures were deleted.

## Gate

- `just check` — **passed** for PR-38 (format, all-target/all-feature Clippy with warnings denied,
  499 listed workspace tests/doctests, 0 failures).
- `cargo llvm-cov --workspace --all-features --summary-only --quiet` — **passed**; 94.27%
  repository line coverage, above the 94.00% Phase N baseline. `workspace.rs` is 97.08%,
  `core/mod.rs` is 90.40%, and `filesync/session.rs` is 91.87% line-covered.
- `cargo bench --features filesync --bench performance -- workspace_hierarchy --sample-size 10
  --measurement-time 0.2` — **passed** for wide/deep page sizes 1/32/256, recursive/node-local
  digest controls, and the one-leaf update/root-read case; measurements are recorded above.
- `cargo check --features filesync --bench performance` — **passed** during implementation; the
  final benchmark target also compiled under the all-target Clippy gate.
- `just check` — **passed** for Phase N (format, all-target/all-feature Clippy with warnings denied,
  495 listed workspace tests/doctests, 0 failures)
- `cargo llvm-cov --workspace --all-features --summary-only --quiet` — **passed**; 94.00%
  repository line coverage, above the 93.68% Phase M baseline. `workspace.rs` is 97.87% and
  `filesync/session.rs` is 91.90% line-covered.
- `cargo bench --bench performance workspace_projection -- --measurement-time 0.2
  --warm-up-time 0.1 --noplot` — **passed** for the complete 100/1,000/10,000 × 1/8/32 matrix.
  The frozen 10k one-block semantic gate measured 7.47 µs and 31,479 allocated/1,182 output bytes,
  versus 12.085 ms and 70,241,737 allocated/369,998 output bytes for full serialization.
- `cargo bench --bench performance --no-run` — **passed** with the counting allocator and all
  projection ablation controls compiled.
- `just check` — **passed** after PR-32/33/34/35 audit (461 test-results, 0 warnings; +4 tests: multi-lease min-ack + duplicate-lease checkpoint, op-segment magic/version/ordering/gap)
- `just check` — **passed** for Phase L (format, all-target/all-feature Clippy with warnings denied,
  457 workspace tests, and 7 doctests)
- `cargo llvm-cov --workspace --all-features --summary-only --quiet` — **passed**; 93.07%
  repository line coverage, above the Phase K 93.01% baseline. New checkpoint/rebase modules are
  94.19–97.46% line-covered, storage is 94.98%, and the path-scoped filesync session is 90.56%.
- `cargo bench --bench performance checkpoint_history -- --sample-size 10 --measurement-time 0.2`
  — **passed**; control/treatment results are recorded in the Phase L outcome.
- `cargo test --test workspace_fixture` — **passed**; committed versioned fixture matches current
  public DTO serialization. The sibling `md-mcp` fixture consumer and joint release gate remain
  explicitly unverified in this repository-only slice.
- `just check` — **passed** after PR-29/30/31 audit (447 test-results, 0 warnings; +4 tests: batch StaleRevision/TOCTOU, deleted-summary, orphan-pending sweep; orphan-`.pending` cleanup + doc-comment fixes)
- `just check` — **passed** for Phase J (format, all-target/all-feature Clippy with warnings denied, full workspace tests, and 7 doctests)
- `cargo llvm-cov --workspace --all-features --summary-only` — **passed** for Phase J; 92.98% repository line coverage, above the required Phase I 92.82% no-line-regression baseline. Region coverage is 91.02%, down 0.13 points from 91.15%; it is reported for transparency but is not the phase gate. The Phase J semantic modules are 100% (`frontmatter.rs`) and 97.81% (`inline.rs`) line-covered.
- `just check` — **passed** after PR-21/22/23 audit (403 test-results, 0 warnings; +4 tests: export idempotence/nested-dir, lossless nested-root/unicode; removed misleading `RevisionToken: Ord`; gated the export test seam behind `cfg(test)`)
- `cargo test --test workspace_contract --test doc_lossless_source --test filesync_export` — **passed** for PR-21–PR-23 (14 tests: persistent identity and opaque contract types, source-local exact serialization, stale refresh rejection, and durable export)
- `just check` — **passed** for Phase I (format, all-target/all-feature Clippy with warnings denied, full workspace tests, and 7 doctests)
- `cargo llvm-cov --workspace --all-features --summary-only` — **passed** for Phase I; 92.82% repository line coverage / 91.15% region coverage, above the 92.12% / 90.69% prior baseline. The main changed production files are 100% line-covered (`workspace.rs`), 98.77% (`doc/source.rs`), 99.17% (`doc/parser.rs`), 90.28% (`doc/mod.rs`), and 92.21% (`filesync/session.rs`); snapshot source-state integration is exercised by JSON and persisted-session reopen tests.
- `cargo test --test filesync_vault_session` — **passed** for the Phase D exchange follow-up (5 tests)
- `just check` — **passed** after audit (0 warnings; README wired into the doctest harness — 6 doctests, all green under `-Dwarnings`)
- `just check` — **passed** for the Phase D exchange follow-up (377 tests, 0 warnings)
- `cargo llvm-cov --workspace --all-features --summary-only` — **passed**; 92.12% repository line coverage / 90.69% region coverage, up from 92.09% lines with the same 560 missed lines; every new `VaultSession` exchange line is covered
- `cargo test --test sync` — **passed** for PR-20 (11 CLI tests, including the two new red/green workflow tests)
- `just check` — **passed** for PR-20 (376 tests, 0 warnings)
- `cargo llvm-cov --workspace --all-features --summary-only` — **passed** for PR-20; 92.09% repository line coverage / 90.72% region coverage, up from 92.03% / 90.66%, with missed lines reduced from 565 to 560; all 7 CLI functions executed
- `cargo test -p md-crdt-ffi --test packaging_contract` — **passed**; unpublished/no-native-artifact/no-placeholder/README contract enforced
- `just check` — **passed** after PR-19 audit (374 tests, 0 warnings; generalized the API-empty contract assertion)
- `just check` — **passed** for PR-19 (374 tests, 0 warnings)
- `cargo llvm-cov --workspace --all-features --summary-only` — **passed** for PR-19; 92.03% repository line coverage / 90.66% region coverage and the same 565 uncovered lines as PR-18. The 0.01-point rounded ratio change is solely the denominator effect of deleting seven fully covered template lines; no production line became uncovered
- `just check` — **passed** after PR-18 audit (374 tests, 0 warnings; strengthened `module_boundaries.rs` into a compile-level façade guard)
- `just check` — **passed** for PR-18 (372 tests, 0 warnings; format and all-target/all-feature Clippy green)
- `cargo llvm-cov --workspace --all-features --summary-only` — **passed** for PR-18; 92.04% repository line coverage / 90.67% region coverage, with changed production files from 90.23% to 100% line coverage
- `cargo bench --bench performance --no-run` and `--features sequence_incremental --no-run` — **passed**; both complete Criterion configurations compile in the bench profile
- Targeted `session_insert_text` Criterion run in both configurations — **passed**; 1k/10k results recorded above
- `just check` — **passed** after PR-17 audit (365 tests, 0 warnings; benchmark target linted via `--all-targets`)
- `cargo bench --bench performance` and `… --features sequence_incremental` — **both executed to completion** during audit (all 14 cases, EXIT 0, no panic/divergence)
- `cargo llvm-cov --workspace --all-features --summary-only` — **passed** for PR-17; 90.82% repository line coverage / 89.54% region coverage, with no regression
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
