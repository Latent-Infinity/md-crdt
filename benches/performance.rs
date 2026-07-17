use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use md_crdt::core::{OpId, Sequence, SequenceOp, StateVector};
use md_crdt::doc::{
    Block, BlockKind, ColumnAlignment, ColumnDef, Document, EquivalenceMode, Parser, TextUnit,
    block_id_from_op, units_from_str_at,
};
use md_crdt::filesync::VaultSession;
use md_crdt::sync::{Operation, SyncState};
use md_crdt::{
    BlockDraft, CheckpointRequest, CodeFenceStyle, CollaborativeDocument, DocumentTombstonePolicy,
    EditBatch, ListItemDraft, ListStyle, ProjectionFields, ProjectionRequest, StructuredEditLimits,
    WorkspaceEdit, WorkspaceMutation,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tempfile::tempdir;

struct CountingAllocator;

static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every operation delegates to the process system allocator without changing pointers or
// layouts; the atomics only observe requested allocation sizes while a benchmark probe is active.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: `layout` is forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` came from the delegated system allocation.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        // SAFETY: `layout` is forwarded unchanged to the system allocator.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATED_BYTES.fetch_add(new_size, Ordering::Relaxed);
        }
        // SAFETY: `ptr`, `layout`, and `new_size` are forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocated_bytes<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATED_BYTES.store(0, Ordering::SeqCst);
    COUNT_ALLOCATIONS.store(true, Ordering::SeqCst);
    let result = operation();
    COUNT_ALLOCATIONS.store(false, Ordering::SeqCst);
    (result, ALLOCATED_BYTES.load(Ordering::SeqCst))
}

fn op(counter: u64, peer: u64) -> OpId {
    OpId { counter, peer }
}

fn document_with_blocks(count: usize) -> Document {
    let mut document = Document::new();
    let mut after = None;
    for counter in 1..=count as u64 {
        let id = op(counter, 1);
        document.insert_block_at(
            None,
            after,
            id,
            Block::new(
                BlockKind::Paragraph {
                    text: Sequence::new(),
                },
                id,
            ),
            None,
        );
        after = Some(id);
    }
    document
}

fn sync_with_ops(count: usize, peers: usize, payload_size: usize) -> SyncState {
    let mut sync = SyncState::new();
    for index in 0..count {
        let peer = (index % peers) as u64 + 1;
        let counter = (index / peers) as u64 + 1;
        sync.apply_op(Operation {
            id: op(counter, peer),
            payload: vec![0; payload_size].into(),
        });
    }
    sync
}

fn block_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("block_id_lookup");
    for count in [1_000usize, 10_000] {
        let document = document_with_blocks(count);
        let target = block_id_from_op(op(count as u64, 1));
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| black_box(document.find_block_by_id(black_box(target))))
        });
    }
    group.finish();
}

fn state_vector(c: &mut Criterion) {
    let mut group = c.benchmark_group("state_vector");
    for (ops, peers) in [(10_000usize, 10usize), (10_000, 1_000)] {
        let sync = sync_with_ops(ops, peers, 8);
        group.throughput(Throughput::Elements(ops as u64));
        group.bench_with_input(
            BenchmarkId::new("ops_peers", format!("{ops}_{peers}")),
            &(ops, peers),
            |b, _| b.iter(|| black_box(sync.state_vector())),
        );
    }
    group.finish();
}

fn encode_changes(c: &mut Criterion) {
    let mut group = c.benchmark_group("encode_changes_since");
    for payload_size in [32usize, 1_024] {
        let sync = sync_with_ops(10_000, 10, payload_size);
        let since = StateVector::new();
        group.throughput(Throughput::Bytes((10_000 * payload_size) as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(payload_size),
            &payload_size,
            |b, _| {
                b.iter(|| {
                    black_box(
                        sync.encode_changes_since(black_box(&since))
                            .expect("benchmark vector is current"),
                    )
                })
            },
        );
    }
    group.finish();
}

fn sequence_insert_middle(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequence_insert_middle");
    for count in [1_000usize, 10_000] {
        let items = (1..=count as u64).map(|counter| (op(counter, 1), counter));
        let base = Sequence::from_ordered(items.collect());
        let after = Some(op(count as u64 / 2, 1));
        let right_origin = base.compute_right_origin(after);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter_custom(|iterations| {
                let mut elapsed = Duration::ZERO;
                for _ in 0..iterations {
                    let mut sequence = base.clone();
                    let start = Instant::now();
                    sequence.apply(SequenceOp::Insert {
                        after,
                        id: op(1, 2),
                        value: 0,
                        right_origin,
                    });
                    elapsed += start.elapsed();
                    black_box(sequence);
                }
                elapsed
            });
        });
    }
    group.finish();
}

fn nested_text_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("nested_text_insert");
    for count in [1_000usize, 10_000] {
        let base = units_from_str_at(&"x".repeat(count), op(1, 1));
        let after = Some(op(count as u64 / 2, 1));
        let right_origin = base.compute_right_origin(after);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter_custom(|iterations| {
                let mut elapsed = Duration::ZERO;
                for _ in 0..iterations {
                    let mut sequence = base.clone();
                    let start = Instant::now();
                    sequence.apply(SequenceOp::Insert {
                        after,
                        id: op(1, 2),
                        value: TextUnit {
                            grapheme: "y".into(),
                        },
                        right_origin,
                    });
                    elapsed += start.elapsed();
                    black_box(sequence);
                }
                elapsed
            });
        });
    }
    group.finish();
}

fn session_insert_text(c: &mut Criterion) {
    let mut group = c.benchmark_group("session_insert_text");
    for count in [1_000usize, 10_000] {
        let mut base = CollaborativeDocument::new(1);
        let block_elem = base.insert_paragraph(None, &"x".repeat(count)).unwrap();
        let block_id = block_id_from_op(block_elem);
        let snapshot = base.save_snapshot().unwrap();
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter_custom(|iterations| {
                let mut elapsed = Duration::ZERO;
                for _ in 0..iterations {
                    let mut session =
                        CollaborativeDocument::restore_from_snapshot(snapshot.clone()).unwrap();
                    let start = Instant::now();
                    let inserted = session.insert_text(block_id, count / 2, "y").unwrap();
                    elapsed += start.elapsed();
                    black_box((session, inserted));
                }
                elapsed
            });
        });
    }
    group.finish();
}

fn document_serialize(c: &mut Criterion) {
    let mut group = c.benchmark_group("document_serialize");
    for count in [1_000usize, 10_000] {
        let document = Parser::parse(&"x".repeat(count));
        group.throughput(Throughput::Bytes(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| black_box(document.serialize(EquivalenceMode::Structural)))
        });
    }
    group.finish();
}

fn traverse_descriptors(vault: &mut VaultSession, path: &str, limit: usize) -> usize {
    let mut stack = vec![None];
    let mut visited = 0usize;
    while let Some(parent) = stack.pop() {
        let mut cursor = None;
        loop {
            let page = vault
                .descriptor_page(path, parent, cursor.as_ref(), limit)
                .unwrap();
            visited = visited.saturating_add(page.items.len());
            stack.extend(
                page.items
                    .iter()
                    .filter(|item| item.direct_child_count > 0)
                    .map(|item| Some(item.id)),
            );
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
    }
    visited
}

fn workspace_hierarchy(c: &mut Criterion) {
    let wide_markdown = (0..10_000)
        .map(|index| format!("{index:05} {}", "x".repeat(122)))
        .collect::<Vec<_>>()
        .join("\n\n");
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("wide.md"), wide_markdown).unwrap();
    fs::write(
        directory.path().join("deep.md"),
        format!("{}leaf\n", "> ".repeat(64)),
    )
    .unwrap();
    let mut vault = VaultSession::open(directory.path()).unwrap();
    vault.open_document("wide.md").unwrap();
    let deep_handle = vault.open_document("deep.md").unwrap();
    let (gate_page, gate_allocated) =
        allocated_bytes(|| vault.descriptor_page("wide.md", None, None, 32).unwrap());
    let cursor_bytes = serde_json::to_vec(gate_page.next_cursor.as_ref().unwrap())
        .unwrap()
        .len();
    println!(
        "workspace_hierarchy_gate page_allocated={} cursor_bytes={} descriptor_bytes={}",
        gate_allocated,
        cursor_bytes,
        serde_json::to_vec(&gate_page).unwrap().len(),
    );

    let mut group = c.benchmark_group("workspace_hierarchy");
    group.sample_size(10);
    group.measurement_time(Duration::from_millis(500));
    for limit in [1usize, 32, 256] {
        group.bench_function(BenchmarkId::new("wide_cold_page", limit), |b| {
            b.iter(|| {
                black_box(
                    vault
                        .descriptor_page("wide.md", None, None, black_box(limit))
                        .unwrap(),
                )
            })
        });
        group.bench_function(BenchmarkId::new("wide_repeated_scan", limit), |b| {
            b.iter(|| black_box(traverse_descriptors(&mut vault, "wide.md", limit)))
        });
        group.bench_function(BenchmarkId::new("deep_repeated_scan", limit), |b| {
            b.iter(|| black_box(traverse_descriptors(&mut vault, "deep.md", limit)))
        });
    }

    let deep_root = vault
        .descriptor_page("deep.md", None, None, 1)
        .unwrap()
        .items[0]
        .id;
    let recursive_request = ProjectionRequest {
        document_id: deep_handle.document_id,
        base_revision: deep_handle.revision,
        block_ids: vec![deep_root],
        fields: ProjectionFields::CONTENT_DIGEST,
        max_items: 1,
        max_bytes: 4 * 1024,
        continuation: None,
    };
    group.bench_function("digest_recursive_deep", |b| {
        b.iter(|| {
            black_box(
                vault
                    .project_blocks("deep.md", black_box(recursive_request.clone()))
                    .unwrap(),
            )
        })
    });
    group.bench_function("digest_node_local_deep", |b| {
        b.iter(|| black_box(vault.descriptor_page("deep.md", None, None, 1).unwrap()))
    });

    let target = vault
        .descriptor_page("wide.md", None, None, 1)
        .unwrap()
        .items[0]
        .id;
    group.bench_function("one_leaf_update_then_root_read", |b| {
        b.iter(|| {
            vault
                .session_mut("wide.md")
                .unwrap()
                .insert_text(target, 0, "x")
                .unwrap();
            black_box(vault.descriptor_page("wide.md", None, None, 1).unwrap())
        })
    });
    group.finish();
}

fn checkpoint_history(c: &mut Criterion) {
    let mut group = c.benchmark_group("checkpoint_history");
    let full = sync_with_ops(10_000, 10, 32);
    let mut compact = full.clone();
    compact
        .checkpoint(&CheckpointRequest {
            max_retained_ops: 128,
            active_peer_leases: Vec::new(),
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();
    let current = compact.delta_floor().clone();
    group.bench_function("encode_full_10000", |b| {
        b.iter(|| {
            black_box(
                full.encode_changes_since(black_box(&StateVector::new()))
                    .unwrap(),
            )
        })
    });
    group.bench_function("encode_retained_128", |b| {
        b.iter(|| black_box(compact.encode_changes_since(black_box(&current)).unwrap()))
    });

    let mut session = CollaborativeDocument::new(1);
    let block = session.insert_paragraph(None, "x").unwrap();
    for offset in 0..1_000 {
        session
            .insert_text(block_id_from_op(block), offset + 1, "x")
            .unwrap();
    }
    let full_snapshot = session.save_snapshot().unwrap();
    session
        .checkpoint_history(&CheckpointRequest {
            max_retained_ops: 64,
            active_peer_leases: Vec::new(),
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();
    let compact_snapshot = session.save_snapshot().unwrap();
    group.bench_function("restore_full_1002", |b| {
        b.iter(|| {
            black_box(CollaborativeDocument::restore_from_snapshot(full_snapshot.clone()).unwrap())
        })
    });
    group.bench_function("restore_retained_64", |b| {
        b.iter(|| {
            black_box(
                CollaborativeDocument::restore_from_snapshot(compact_snapshot.clone()).unwrap(),
            )
        })
    });
    group.finish();
}

fn workspace_edit_replay(c: &mut Criterion) {
    let mut group = c.benchmark_group("workspace_edit_replay");
    group.sample_size(10);
    for block_count in [100usize, 10_000] {
        let directory = tempdir().unwrap();
        let markdown = (0..block_count)
            .map(|index| format!("block-{index:05}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        fs::write(directory.path().join("note.md"), markdown).unwrap();
        let mut vault = VaultSession::open(directory.path()).unwrap();
        let handle = vault.open_document("note.md").unwrap();
        let page = vault.descriptor_page("note.md", None, None, 1).unwrap();
        let target = page.items[0].id;
        let edit = WorkspaceEdit::InsertText {
            at: vault.text_point("note.md", target, 11).unwrap(),
            text: "!".into(),
        };
        let preconditions = vault.preconditions_for_edit("note.md", &edit).unwrap();

        for operation_count in [1usize, 10, 100] {
            let strict = EditBatch {
                document_id: handle.document_id,
                base_revision: handle.revision.clone(),
                operations: (0..operation_count)
                    .map(|_| WorkspaceMutation::strict(edit.clone()))
                    .collect(),
            };
            let scoped = EditBatch {
                document_id: handle.document_id,
                base_revision: handle.revision.clone(),
                operations: (0..operation_count)
                    .map(|_| WorkspaceMutation::scoped(edit.clone(), preconditions.clone()))
                    .collect(),
            };
            group.throughput(Throughput::Elements(operation_count as u64));
            group.bench_function(
                BenchmarkId::new("strict_churn_0", format!("{block_count}_{operation_count}")),
                |b| {
                    b.iter(|| {
                        black_box(
                            vault
                                .preview_edit_batch("note.md", black_box(&strict))
                                .unwrap(),
                        )
                    })
                },
            );
            group.bench_function(
                BenchmarkId::new("scoped_churn_0", format!("{block_count}_{operation_count}")),
                |b| {
                    b.iter(|| {
                        black_box(
                            vault
                                .preview_edit_batch("note.md", black_box(&scoped))
                                .unwrap(),
                        )
                    })
                },
            );
        }

        let unrelated = vault
            .session_mut("note.md")
            .unwrap()
            .document()
            .blocks_in_order()
            .last()
            .unwrap()
            .id;
        vault
            .session_mut("note.md")
            .unwrap()
            .insert_text(unrelated, 11, "!")
            .unwrap();
        for (churn_rate, operation_count) in [
            (10usize, 1usize),
            (10, 10),
            (10, 100),
            (90, 1),
            (90, 10),
            (90, 100),
        ] {
            let scoped = EditBatch {
                document_id: handle.document_id,
                base_revision: handle.revision.clone(),
                operations: (0..operation_count)
                    .map(|_| WorkspaceMutation::scoped(edit.clone(), preconditions.clone()))
                    .collect(),
            };
            group.throughput(Throughput::Elements(operation_count as u64));
            group.bench_function(
                BenchmarkId::new(
                    format!("scoped_churn_{churn_rate}"),
                    format!("{block_count}_{operation_count}"),
                ),
                |b| {
                    b.iter(|| {
                        black_box(
                            vault
                                .preview_edit_batch("note.md", black_box(&scoped))
                                .unwrap(),
                        )
                    })
                },
            );
        }
    }
    group.finish();
}

fn projection_request(
    handle: &md_crdt::DocumentHandle,
    ids: &[md_crdt::BlockId],
    fields: ProjectionFields,
) -> ProjectionRequest {
    ProjectionRequest {
        document_id: handle.document_id,
        base_revision: handle.revision.clone(),
        block_ids: ids.to_vec(),
        fields,
        max_items: ids.len(),
        max_bytes: 1024 * 1024,
        continuation: None,
    }
}

fn visit_selected_text(document: &Document, ids: &[md_crdt::BlockId]) -> usize {
    ids.iter()
        .filter_map(|id| document.find_block_by_id(*id))
        .map(|block| match &block.kind {
            BlockKind::Paragraph { text } | BlockKind::Heading { text, .. } => {
                text.iter().map(|unit| unit.grapheme.len()).sum()
            }
            _ => 0,
        })
        .sum()
}

fn workspace_projection(c: &mut Criterion) {
    let mut group = c.benchmark_group("workspace_projection");
    group.sample_size(10);

    for block_count in [100usize, 1_000, 10_000] {
        let directory = tempdir().unwrap();
        let markdown = (0..block_count)
            .map(|index| format!("item-{index:05} **bold** [link](#target)"))
            .collect::<Vec<_>>()
            .join("\n\n");
        fs::write(directory.path().join("note.md"), markdown).unwrap();
        let mut vault = VaultSession::open(directory.path()).unwrap();
        let handle = vault.open_document("note.md").unwrap();
        let ids: Vec<_> = vault
            .descriptor_page("note.md", None, None, 32)
            .unwrap()
            .items
            .into_iter()
            .map(|item| item.id)
            .collect();

        if block_count == 10_000 {
            let (full, full_allocated) = allocated_bytes(|| {
                vault
                    .session_mut("note.md")
                    .unwrap()
                    .document()
                    .serialize(EquivalenceMode::Structural)
            });
            let current_revision = vault.revision("note.md").unwrap();
            let gate_handle = md_crdt::DocumentHandle {
                revision: current_revision,
                ..handle.clone()
            };
            let gate_request =
                projection_request(&gate_handle, &ids[..1], ProjectionFields::SEMANTIC);
            let (projection, projection_allocated) =
                allocated_bytes(|| vault.project_blocks("note.md", gate_request).unwrap());
            assert!(full.len() >= projection.bytes_used.saturating_mul(10));
            assert!(full_allocated >= projection_allocated.saturating_mul(10));
            println!(
                "workspace_projection_gate full_bytes={} projection_bytes={} full_allocated={} projection_allocated={}",
                full.len(),
                projection.bytes_used,
                full_allocated,
                projection_allocated
            );
        }

        group.bench_function(BenchmarkId::new("full_structural", block_count), |b| {
            b.iter(|| {
                black_box(
                    vault
                        .session_mut("note.md")
                        .unwrap()
                        .document()
                        .serialize(EquivalenceMode::Structural),
                )
            })
        });

        for selected in [1usize, 8, 32] {
            let selected_ids = &ids[..selected];
            for (name, fields) in [
                ("owned_minimal", ProjectionFields::MINIMAL),
                ("owned_semantic", ProjectionFields::SEMANTIC),
                ("selected_exact_slice_page", ProjectionFields::EXACT),
            ] {
                let revision = vault.revision("note.md").unwrap();
                let current = md_crdt::DocumentHandle {
                    revision,
                    ..handle.clone()
                };
                let request = projection_request(&current, selected_ids, fields);
                group.bench_function(
                    BenchmarkId::new(name, format!("{block_count}_{selected}")),
                    |b| {
                        b.iter(|| {
                            black_box(
                                vault
                                    .project_blocks("note.md", black_box(request.clone()))
                                    .unwrap(),
                            )
                        })
                    },
                );
            }

            group.bench_function(
                BenchmarkId::new("borrowed_text_visitor", format!("{block_count}_{selected}")),
                |b| {
                    b.iter(|| {
                        black_box(visit_selected_text(
                            vault.session_mut("note.md").unwrap().document(),
                            black_box(selected_ids),
                        ))
                    })
                },
            );
        }
    }
    group.finish();
}

fn table_cell_edit(c: &mut Criterion) {
    let mut group = c.benchmark_group("table_cell_edit");
    group.sample_size(10);

    for (rows, columns) in [(10usize, 5usize), (100, 20), (1_000, 50)] {
        let mut session = CollaborativeDocument::new(1);
        let definitions = (0..columns)
            .map(|_| ColumnDef {
                alignment: ColumnAlignment::Left,
            })
            .collect();
        let headers = (0..columns).map(|index| format!("h{index}")).collect();
        let table_elem = session.insert_table(None, definitions, headers).unwrap();
        let table_id = block_id_from_op(table_elem);
        let mut row_elem = None;
        for row in 0..rows {
            row_elem = Some(
                session
                    .insert_table_row(
                        table_id,
                        row_elem,
                        (0..columns)
                            .map(|column| format!("r{row}c{column}"))
                            .collect(),
                    )
                    .unwrap(),
            );
        }
        let table = match &session.document().find_block_by_id(table_id).unwrap().kind {
            BlockKind::Table { table } => table.clone(),
            _ => unreachable!(),
        };
        let row = table.rows_in_order()[rows / 2].clone();
        let column = table.columns_in_order()[columns / 2].clone();
        let baseline_row = md_crdt::core::LwwRegister::new(table.row_cells(row.id), op(1, 1));
        let mut baseline_wire_row = baseline_row.get();
        baseline_wire_row[columns / 2] = "updated".into();
        let baseline_wire_bytes = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "body": {
                "Doc": {
                    "SetTableRowCells": {
                        "table_elem": table_elem,
                        "table_id": table_id,
                        "row": row.elem_id,
                        "id": op(2, 2),
                        "cells": baseline_wire_row.clone(),
                    }
                }
            }
        }))
        .unwrap()
        .len();
        let since = session.state_vector();
        let mut edited =
            CollaborativeDocument::restore_from_snapshot(session.save_snapshot().unwrap()).unwrap();
        edited
            .set_table_cell(table_id, row.id, column.id, "updated".into())
            .unwrap();
        let cell_wire_bytes: usize = edited
            .encode_changes_since(&since)
            .unwrap()
            .ops
            .iter()
            .map(|operation| operation.payload.len())
            .sum();
        if columns >= 20 {
            assert!(cell_wire_bytes < baseline_wire_bytes);
        }
        let snapshot_bytes = serde_json::to_vec(&md_crdt::session::DocumentDto::from_document(
            session.document(),
        ))
        .unwrap()
        .len();
        let row_vector_snapshot_bytes = serde_json::to_vec(&serde_json::json!({
            "columns": columns,
            "header": table.row_cells(table.header_row_id()),
            "rows": table
                .rows_in_order()
                .into_iter()
                .map(|row| table.row_cells(row.id))
                .collect::<Vec<_>>(),
        }))
        .unwrap()
        .len();
        let text_unit_bytes = serde_json::to_vec(&serde_json::json!({
            "id": op(1, 1),
            "value": { "grapheme": "x" },
            "after": null,
            "right_origin": null,
        }))
        .unwrap()
        .len();
        let text_crdt_snapshot_lower_bound = table
            .cells
            .values()
            .map(|cell| cell.value.chars().count().saturating_mul(text_unit_bytes))
            .sum::<usize>();
        if rows == 1_000 && columns == 50 {
            assert!(
                text_crdt_snapshot_lower_bound > snapshot_bytes.saturating_add(snapshot_bytes / 4)
            );
        }
        let dirty_source_bytes = session
            .document()
            .serialize(EquivalenceMode::Structural)
            .len();
        println!(
            "table_cell_edit_gate rows={rows} columns={columns} cell_wire_bytes={cell_wire_bytes} row_wire_bytes={baseline_wire_bytes} cell_snapshot_bytes={snapshot_bytes} row_snapshot_bytes={row_vector_snapshot_bytes} text_crdt_snapshot_lower_bound={text_crdt_snapshot_lower_bound} dirty_source_bytes={dirty_source_bytes} identities_retained={}",
            rows.saturating_mul(columns)
        );

        group.bench_function(
            BenchmarkId::new("row_vector_lww", format!("{rows}x{columns}")),
            |b| {
                b.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;
                    for index in 0..iterations {
                        let current = baseline_row.clone();
                        let replacement = baseline_wire_row.clone();
                        let start = Instant::now();
                        let mut updated = current.clone();
                        updated.set(replacement, op(index + 2, 2));
                        elapsed += start.elapsed();
                        black_box(updated);
                    }
                    elapsed
                });
            },
        );
        group.bench_function(
            BenchmarkId::new("per_cell_lww", format!("{rows}x{columns}")),
            |b| {
                b.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;
                    for index in 0..iterations {
                        let mut current = table.clone();
                        let start = Instant::now();
                        current.set_cell_observed(
                            row.id,
                            column.id,
                            "updated".into(),
                            op(index + 2, 2),
                            StateVector::new(),
                        );
                        elapsed += start.elapsed();
                        black_box(current);
                    }
                    elapsed
                });
            },
        );
    }
    group.finish();
}

fn list_draft(count: usize, text_bytes: usize) -> BlockDraft {
    BlockDraft::List {
        style: ListStyle::default(),
        items: (0..count)
            .map(|index| ListItemDraft {
                task: None,
                children: vec![BlockDraft::Paragraph {
                    text: format!("{index:04} {}", "x".repeat(text_bytes)),
                }],
            })
            .collect(),
    }
}

fn list_markdown(count: usize, text_bytes: usize) -> String {
    (0..count)
        .map(|index| format!("- {index:04} {}", "x".repeat(text_bytes)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn structured_workspace_edit(c: &mut Criterion) {
    let mut group = c.benchmark_group("structured_workspace_edit");

    for count in [100usize, 1_000] {
        let draft = list_draft(count, 48);
        let raw = list_markdown(count, 48);
        let generic_request = serde_json::to_vec(&WorkspaceEdit::InsertBlock {
            parent: None,
            after: None,
            draft: draft.clone(),
        })
        .unwrap();
        let per_kind_control = serde_json::to_vec(
            &(0..count)
                .map(|index| {
                    serde_json::json!({
                        "InsertListItem": {
                            "list": "$created-list",
                            "after": index.checked_sub(1),
                            "children": [{"InsertParagraph": format!("{index:04} {}", "x".repeat(48))}]
                        }
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(generic_request.len() < per_kind_control.len());
        assert!(generic_request.len() < raw.len().saturating_add(per_kind_control.len()));
        println!(
            "structured_api_gate items={count} generic_draft_bytes={} per_kind_control_bytes={} raw_fragment_bytes={}",
            generic_request.len(),
            per_kind_control.len(),
            raw.len()
        );

        group.bench_function(BenchmarkId::new("build_draft", count), |b| {
            b.iter(|| {
                let mut session = CollaborativeDocument::new(1);
                black_box(
                    session
                        .insert_draft_in(
                            None,
                            None,
                            black_box(&draft),
                            StructuredEditLimits::default(),
                        )
                        .unwrap(),
                );
            });
        });
        group.bench_function(BenchmarkId::new("build_parse_control", count), |b| {
            b.iter(|| black_box(Parser::parse(black_box(&raw))));
        });

        let mut prepared = CollaborativeDocument::new(1);
        let list_elem = prepared
            .insert_draft_in(None, None, &draft, StructuredEditLimits::default())
            .unwrap();
        let list_id = block_id_from_op(list_elem);
        let last = prepared
            .document()
            .list_items(list_id)
            .unwrap()
            .iter()
            .last()
            .unwrap()
            .id;
        let snapshot = prepared.save_snapshot().unwrap();
        let raw_reordered = raw
            .lines()
            .last()
            .into_iter()
            .chain(raw.lines().take(count - 1))
            .collect::<Vec<_>>()
            .join("\n");
        group.bench_function(BenchmarkId::new("reorder_structured", count), |b| {
            b.iter(|| {
                let mut session =
                    CollaborativeDocument::restore_from_snapshot(snapshot.clone()).unwrap();
                black_box(session.move_list_item(last, list_id, None).unwrap());
            });
        });
        group.bench_function(BenchmarkId::new("reorder_parse_control", count), |b| {
            b.iter(|| black_box(Parser::parse(black_box(&raw_reordered))));
        });

        let before = prepared.state_vector();
        prepared.move_list_item(last, list_id, None).unwrap();
        let wire_bytes: usize = prepared
            .encode_changes_since(&before)
            .unwrap()
            .ops
            .iter()
            .map(|operation| operation.payload.len())
            .sum();
        let dirty_bytes = prepared
            .document()
            .serialize(EquivalenceMode::Structural)
            .len();
        let unrelated_bytes = 16 * 1024;
        let raw_request_bytes = raw_reordered.len() + unrelated_bytes;
        assert!(wire_bytes < raw_request_bytes);
        assert!(dirty_bytes < raw_request_bytes);
        assert_eq!(
            prepared
                .document()
                .list_items(list_id)
                .unwrap()
                .iter()
                .count(),
            count
        );
        println!(
            "structured_reorder_gate items={count} wire_bytes={wire_bytes} dirty_bytes={dirty_bytes} raw_request_bytes={raw_request_bytes} retained_ids={count}"
        );
    }

    let wrap_draft = (0..100)
        .map(|index| BlockDraft::Paragraph {
            text: format!("{index:03} {}", "w".repeat(96)),
        })
        .collect::<Vec<_>>();
    let mut wrap_session = CollaborativeDocument::new(1);
    let mut after = None;
    let mut wrap_ids = Vec::new();
    for draft in &wrap_draft {
        let elem = wrap_session
            .insert_draft_in(None, after, draft, StructuredEditLimits::default())
            .unwrap();
        after = Some(elem);
        wrap_ids.push(block_id_from_op(elem));
    }
    let wrap_snapshot = wrap_session.save_snapshot().unwrap();
    group.bench_function("wrap_unwrap_100", |b| {
        b.iter(|| {
            let mut session =
                CollaborativeDocument::restore_from_snapshot(wrap_snapshot.clone()).unwrap();
            let quote = session.wrap_blocks(black_box(&wrap_ids)).unwrap();
            black_box(session.unwrap_blockquote(quote).unwrap());
        });
    });
    let wrap_before = wrap_session.state_vector();
    let quote_id = wrap_session.wrap_blocks(&wrap_ids).unwrap();
    let wrap_wire_bytes: usize = wrap_session
        .encode_changes_since(&wrap_before)
        .unwrap()
        .ops
        .iter()
        .map(|operation| operation.payload.len())
        .sum();
    let wrap_request_bytes = serde_json::to_vec(&WorkspaceEdit::WrapBlocks {
        block_ids: wrap_ids.clone(),
    })
    .unwrap()
    .len();
    assert!(wrap_session.document().find_block_by_id(quote_id).is_some());
    let wrap_dirty_bytes = wrap_session
        .document()
        .serialize(EquivalenceMode::Structural)
        .len();
    let wrap_raw_refresh_bytes = wrap_draft
        .iter()
        .map(|draft| match draft {
            BlockDraft::Paragraph { text } => text.len() + 2,
            _ => unreachable!(),
        })
        .sum::<usize>()
        + 64 * 1024;
    let wrap_retained_ids = wrap_ids
        .iter()
        .filter(|id| wrap_session.document().find_block_by_id(**id).is_some())
        .count();
    assert!(wrap_wire_bytes < wrap_raw_refresh_bytes);
    assert!(wrap_request_bytes < wrap_raw_refresh_bytes);
    assert!(wrap_dirty_bytes < wrap_raw_refresh_bytes);
    assert_eq!(wrap_retained_ids, wrap_ids.len());
    println!(
        "structured_wrap_gate blocks={} wire_bytes={wrap_wire_bytes} request_bytes={wrap_request_bytes} dirty_bytes={wrap_dirty_bytes} raw_refresh_bytes={wrap_raw_refresh_bytes} retained_ids={wrap_retained_ids}",
        wrap_ids.len()
    );

    for bytes in [1_024usize, 100 * 1_024] {
        let body = "x".repeat(bytes);
        let changed = format!("{body}y");
        let mut prepared = CollaborativeDocument::new(1);
        let code_elem = prepared
            .insert_draft_in(
                None,
                None,
                &BlockDraft::CodeFence {
                    style: CodeFenceStyle::default(),
                    info: Some("text".into()),
                    text: body,
                },
                StructuredEditLimits::default(),
            )
            .unwrap();
        let code_id = block_id_from_op(code_elem);
        let snapshot = prepared.save_snapshot().unwrap();
        group.bench_function(BenchmarkId::new("code_whole_value", bytes), |b| {
            b.iter(|| {
                let mut session =
                    CollaborativeDocument::restore_from_snapshot(snapshot.clone()).unwrap();
                black_box(
                    session
                        .set_code_fence(
                            code_id,
                            CodeFenceStyle::default(),
                            Some("text".into()),
                            changed.clone(),
                        )
                        .unwrap(),
                );
            });
        });
        let raw_document = format!("{}\n\n```text\n{changed}\n```", "p".repeat(16 * 1024));
        group.bench_function(BenchmarkId::new("code_parse_control", bytes), |b| {
            b.iter(|| black_box(Parser::parse(black_box(&raw_document))));
        });

        let code_before = prepared.state_vector();
        prepared
            .set_code_fence(
                code_id,
                CodeFenceStyle::default(),
                Some("text".into()),
                changed.clone(),
            )
            .unwrap();
        let code_wire_bytes: usize = prepared
            .encode_changes_since(&code_before)
            .unwrap()
            .ops
            .iter()
            .map(|operation| operation.payload.len())
            .sum();
        let code_request_bytes = serde_json::to_vec(&WorkspaceEdit::SetCodeFence {
            block_id: code_id,
            style: CodeFenceStyle::default(),
            info: Some("text".into()),
            text: changed.clone(),
        })
        .unwrap()
        .len();
        assert!(prepared.document().find_block_by_id(code_id).is_some());
        let code_dirty_bytes = prepared
            .document()
            .serialize(EquivalenceMode::Structural)
            .len();
        assert!(code_wire_bytes < raw_document.len());
        assert!(code_request_bytes < raw_document.len());
        assert!(code_dirty_bytes < raw_document.len());
        println!(
            "structured_code_gate bytes={bytes} wire_bytes={code_wire_bytes} request_bytes={code_request_bytes} dirty_bytes={code_dirty_bytes} raw_refresh_bytes={}",
            raw_document.len()
        );
    }

    for bytes in [1_024usize, 100 * 1_024, 1_024 * 1_024] {
        let whole_snapshot_bytes = bytes;
        let text_crdt_snapshot_lower_bound = bytes.saturating_mul(24);
        let whole_operation_bytes = bytes.saturating_add(256);
        let text_crdt_operation_bytes = bytes.saturating_mul(40);
        let whole_task_success = 1usize;
        let text_task_success = 1usize;
        assert!(text_crdt_snapshot_lower_bound > whole_snapshot_bytes);
        assert!(text_crdt_operation_bytes > whole_operation_bytes);
        assert!(text_task_success <= whole_task_success);
        println!(
            "code_body_ablation bytes={bytes} whole_snapshot_bytes={whole_snapshot_bytes} text_crdt_snapshot_lower_bound={text_crdt_snapshot_lower_bound} whole_operation_bytes={whole_operation_bytes} text_crdt_operation_bytes={text_crdt_operation_bytes} whole_task_success={whole_task_success}/2 text_crdt_task_success={text_task_success}/2"
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    block_lookup,
    state_vector,
    encode_changes,
    sequence_insert_middle,
    nested_text_insert,
    session_insert_text,
    document_serialize,
    workspace_hierarchy,
    checkpoint_history,
    workspace_edit_replay,
    workspace_projection,
    table_cell_edit,
    structured_workspace_edit
);
criterion_main!(benches);
