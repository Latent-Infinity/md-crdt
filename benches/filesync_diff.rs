//! Same-source public-API study of bounded paragraph alignment and vault refresh.
//! `MD_CRDT_DIFF_SCOPE=kernel|refresh` avoids preparing the other measurement scope.
//! `MD_CRDT_DIFF_CASE` selects a case-name substring before any fixture is built.
//! `--alloc-probe kernel|refresh [case-substring]` runs only one-shot probes.
//! Original refusals are labeled separately: admission is not a latency speedup.
//! All cases report operation latency; refusal measures admission-check latency,
//! never nominal input-processing throughput.

#[path = "support/allocations.rs"]
mod allocation_support;

use allocation_support::allocations;
use criterion::{BatchSize, Criterion, criterion_group};
use md_crdt::doc::{BlockKind, paragraph_visible_ids, paragraph_visible_string};
use md_crdt::filesync::{GraphemeStep, VaultError, VaultSession, graphemes_of, lcs_steps};
use md_crdt::limits::InputLimitError;
use std::fs;
use std::hint::black_box;

const SEALED_MATRIX_LIMIT: usize = 4_000_000;

struct Case {
    name: String,
    before: String,
    after: String,
    edits: usize,
    unchanged: bool,
}

fn for_cases(filter: &str, mut visit: impl FnMut(Case)) {
    for count in [16usize, 128, 1_000, 5_000, 50_000] {
        for pattern in ["unique", "repeated", "unicode"] {
            for edit in [
                "unchanged",
                "start_insert",
                "middle_insert",
                "end_insert",
                "start_delete",
                "middle_delete",
                "end_delete",
                "start_substitute",
                "middle_substitute",
                "end_substitute",
                "dissimilar",
            ] {
                let name = format!("{pattern}/{edit}/{count}");
                if !name.contains(filter) {
                    continue;
                }
                let before = match pattern {
                    "unique" => (0..count)
                        .map(|index| char::from_u32(0xf0000 + index as u32).unwrap())
                        .collect::<String>(),
                    "repeated" => "a".repeat(count),
                    "unicode" => ["e\u{301}", "👩\u{200d}💻", "🇺🇸", "🧑🏽\u{200d}🚀", "界"]
                        .into_iter()
                        .cycle()
                        .take(count)
                        .collect::<String>(),
                    _ => unreachable!(),
                };
                let old = graphemes_of(&before);
                assert_eq!(old.len(), count);
                let mut new = old.clone();
                let position = if edit.starts_with("start") {
                    0
                } else if edit.starts_with("middle") {
                    count / 2
                } else if edit.ends_with("insert") {
                    count
                } else {
                    count - 1
                };
                let edits = if edit.ends_with("insert") {
                    new.insert(position, "§");
                    1
                } else if edit.ends_with("delete") {
                    new.remove(position);
                    1
                } else if edit.ends_with("substitute") {
                    new[position] = "§";
                    2
                } else if edit == "dissimilar" {
                    new.fill("§");
                    count * 2
                } else {
                    0
                };
                let after = new.concat();
                visit(Case {
                    name,
                    before,
                    after,
                    edits,
                    unchanged: edit == "unchanged",
                });
            }
        }
    }
}

// Frozen sealed-source oracle, used only outside the measurement boundary and
// only on inputs accepted by the original matrix cap. Backward equality wins;
// backward insertion wins score ties (delete-first after the final reversal).
fn sealed_steps(old: &[&str], new: &[&str]) -> Vec<GraphemeStep> {
    assert!(original_kernel_success(old, new));
    reference_steps(old, new)
}

// Full old recurrence, also used on finite newly admitted proof inputs. Its
// extra matrix allocation is proof-only and never inside a measured operation.
fn reference_steps(old: &[&str], new: &[&str]) -> Vec<GraphemeStep> {
    let n = old.len();
    let m = new.len();
    let stride = m + 1;
    let mut dp = vec![0u32; (n + 1) * stride];
    for i in 0..n {
        for j in 0..m {
            dp[(i + 1) * stride + j + 1] = if old[i] == new[j] {
                dp[i * stride + j] + 1
            } else {
                dp[i * stride + j + 1].max(dp[(i + 1) * stride + j])
            };
        }
    }
    let mut steps = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old[i - 1] == new[j - 1] {
            steps.push(GraphemeStep::Equal {
                old: i - 1,
                new: j - 1,
            });
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i * stride + j - 1] >= dp[(i - 1) * stride + j]) {
            steps.push(GraphemeStep::Insert { new: j - 1 });
            j -= 1;
        } else {
            steps.push(GraphemeStep::Delete { old: i - 1 });
            i -= 1;
        }
    }
    steps.reverse();
    steps
}

fn original_kernel_success(old: &[&str], new: &[&str]) -> bool {
    (old.len() + 1).saturating_mul(new.len() + 1) <= SEALED_MATRIX_LIMIT
}

fn validate_steps(old: &[&str], new: &[&str], steps: &[GraphemeStep], edits: usize) {
    let (mut i, mut j, mut actual_edits) = (0, 0, 0);
    for step in steps {
        match *step {
            GraphemeStep::Equal { old: a, new: b } => {
                assert_eq!((a, b), (i, j));
                assert_eq!(old[a], new[b]);
                i += 1;
                j += 1;
            }
            GraphemeStep::Delete { old: a } => {
                assert_eq!(a, i);
                i += 1;
                actual_edits += 1;
            }
            GraphemeStep::Insert { new: b } => {
                assert_eq!(b, j);
                j += 1;
                actual_edits += 1;
            }
        }
    }
    assert_eq!((i, j), (old.len(), new.len()));
    assert_eq!(actual_edits, edits);
    if original_kernel_success(old, new) {
        assert_eq!(steps, sealed_steps(old, new));
    }
}

fn status<T>(result: &Result<T, InputLimitError>) -> &'static str {
    match result {
        Ok(_) => "success",
        Err(InputLimitError::DiffWork) => "refusal",
        Err(error) => panic!("unexpected alignment error: {error}"),
    }
}

fn kernel_observation(case: &Case, old: &[&str], new: &[&str]) -> String {
    let result = lcs_steps(old, new);
    if let Ok(steps) = &result {
        validate_steps(old, new, steps, case.edits);
    }
    let original = if original_kernel_success(old, new) {
        "success"
    } else {
        "refusal"
    };
    let actual = status(&result);
    if original == "success" {
        assert_eq!(
            actual, "success",
            "previously supported input must stay supported"
        );
    }
    let label = format!("original_{original}/current_{actual}/{}", case.name);
    println!(
        "diff_case scope=kernel name={} original={original} actual={actual} old_graphemes={} new_graphemes={} old_bytes={} new_bytes={} original_cells={} minimum_edits={} output_steps={} output_bytes={}",
        case.name,
        old.len(),
        new.len(),
        case.before.len(),
        case.after.len(),
        (old.len() + 1) * (new.len() + 1),
        case.edits,
        result.as_ref().map_or(0, Vec::len),
        result
            .as_ref()
            .map_or(0, |steps| std::mem::size_of_val(steps.as_slice())),
    );
    label
}

struct RefreshFixture {
    directory: tempfile::TempDir,
    vault: VaultSession,
    block: md_crdt::BlockId,
    old_ids: Vec<md_crdt::OpId>,
}

fn refresh_fixture(case: &Case) -> RefreshFixture {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("note.md"), &case.before).unwrap();
    let mut vault = VaultSession::open(directory.path()).unwrap();
    vault.ingest_all().unwrap();
    let document = vault.session_mut("note.md").unwrap().document();
    let block = document.blocks_in_order()[0];
    let BlockKind::Paragraph { text } = &block.kind else {
        panic!("paragraph expected")
    };
    let old_ids = paragraph_visible_ids(text);
    let block = block.id;
    fs::write(directory.path().join("note.md"), &case.after).unwrap();
    RefreshFixture {
        directory,
        vault,
        block,
        old_ids,
    }
}

fn refresh_observation(case: &Case) -> String {
    let mut fixture = refresh_fixture(case);
    let before = fixture
        .vault
        .session_mut("note.md")
        .unwrap()
        .save_snapshot()
        .unwrap()
        .to_bytes()
        .unwrap();
    let old = graphemes_of(&case.before);
    let new = graphemes_of(&case.after);
    let original = if case.unchanged || original_kernel_success(&old, &new) {
        "success"
    } else {
        "refusal"
    };
    let result = fixture.vault.refresh_markdown("note.md", None, None);
    let actual = match result {
        Ok(outcome) => {
            assert_eq!(outcome.changed, !case.unchanged);
            let document = fixture.vault.session_mut("note.md").unwrap().document();
            let block = document.find_block_by_id(fixture.block).unwrap();
            assert_eq!(document.blocks_in_order().len(), 1);
            let BlockKind::Paragraph { text } = &block.kind else {
                panic!("paragraph expected")
            };
            assert_eq!(paragraph_visible_string(text), case.after);
            let ids = paragraph_visible_ids(text);
            if case.unchanged {
                assert_eq!(ids, fixture.old_ids);
            } else {
                let steps = lcs_steps(&old, &new).unwrap();
                validate_steps(&old, &new, &steps, case.edits);
                for step in steps {
                    match step {
                        GraphemeStep::Equal { old, new } => {
                            assert_eq!(ids[new], fixture.old_ids[old])
                        }
                        GraphemeStep::Insert { new } => {
                            assert!(!fixture.old_ids.contains(&ids[new]))
                        }
                        GraphemeStep::Delete { old } => {
                            assert!(!ids.contains(&fixture.old_ids[old]))
                        }
                    }
                }
            }
            "success"
        }
        Err(VaultError::InputLimit(InputLimitError::DiffWork)) => {
            let after = fixture
                .vault
                .session_mut("note.md")
                .unwrap()
                .save_snapshot()
                .unwrap()
                .to_bytes()
                .unwrap();
            assert_eq!(
                before, after,
                "refusal must preserve the complete live snapshot"
            );
            "refusal"
        }
        Err(error) => panic!("unexpected refresh error: {error}"),
    };
    assert_eq!(
        fs::read_to_string(fixture.directory.path().join("note.md")).unwrap(),
        case.after
    );
    if original == "success" {
        assert_eq!(
            actual, "success",
            "previously supported refresh must stay supported"
        );
    }
    println!(
        "diff_case scope=refresh name={} original={original} actual={actual}",
        case.name
    );
    format!("original_{original}/current_{actual}/{}", case.name)
}

fn scope_enabled(scope: &str) -> bool {
    std::env::var("MD_CRDT_DIFF_SCOPE").map_or(true, |selected| selected == scope)
}

fn kernel(c: &mut Criterion) {
    if !scope_enabled("kernel") {
        return;
    }
    let filter = std::env::var("MD_CRDT_DIFF_CASE").unwrap_or_default();
    let mut group = c.benchmark_group("filesync_diff_kernel");
    for_cases(&filter, |case| {
        let old = graphemes_of(&case.before);
        let new = graphemes_of(&case.after);
        let label = kernel_observation(&case, &old, &new);
        group.bench_function(label, |b| {
            b.iter(|| black_box(lcs_steps(black_box(&old), black_box(&new))))
        });
    });
    group.finish();
}

fn refresh(c: &mut Criterion) {
    if !scope_enabled("refresh") {
        return;
    }
    let filter = std::env::var("MD_CRDT_DIFF_CASE").unwrap_or_default();
    let mut group = c.benchmark_group("filesync_diff_refresh");
    for_cases(&filter, |case| {
        let label = refresh_observation(&case);
        group.bench_function(label, |b| {
            b.iter_batched(
                || refresh_fixture(&case),
                |mut fixture| {
                    let result = fixture.vault.refresh_markdown("note.md", None, None);
                    black_box((fixture, result))
                },
                BatchSize::PerIteration,
            );
        });
    });
    group.finish();
}

fn allocation_probes(scope: &str, filter: &str) {
    assert!(
        scope == "kernel" || scope == "refresh",
        "scope must be kernel or refresh"
    );
    for_cases(filter, |case| {
        if scope == "kernel" {
            let old = graphemes_of(&case.before);
            let new = graphemes_of(&case.after);
            let label = kernel_observation(&case, &old, &new);
            allocations(&format!("diff_kernel/{label}"), || lcs_steps(&old, &new));
        } else {
            let label = refresh_observation(&case);
            let mut fixture = refresh_fixture(&case);
            println!(
                "REFRESH_MEASURE_BEGIN name={} root={}",
                case.name,
                fixture.directory.path().display()
            );
            allocations(&format!("diff_refresh/{label}"), || {
                fixture.vault.refresh_markdown("note.md", None, None)
            });
            println!("REFRESH_MEASURE_END name={}", case.name);
        }
    });
}

criterion_group!(benches, kernel, refresh);
// Proof-only trimming experiments use the frozen oracle, never a production
// candidate. Their correspondence vectors expose why equal-prefix trimming is
// not interchangeable with this sealed source's backward traceback.
fn trimmed_reference(old: &[&str], new: &[&str], prefix: bool) -> Vec<GraphemeStep> {
    let trimmed = if prefix {
        old.iter().zip(new).take_while(|(a, b)| a == b).count()
    } else {
        old.iter()
            .rev()
            .zip(new.iter().rev())
            .take_while(|(a, b)| a == b)
            .count()
    };
    if prefix {
        let mut steps = (0..trimmed)
            .map(|index| GraphemeStep::Equal {
                old: index,
                new: index,
            })
            .collect::<Vec<_>>();
        steps.extend(
            sealed_steps(&old[trimmed..], &new[trimmed..])
                .into_iter()
                .map(|step| match step {
                    GraphemeStep::Equal { old, new } => GraphemeStep::Equal {
                        old: old + trimmed,
                        new: new + trimmed,
                    },
                    GraphemeStep::Delete { old } => GraphemeStep::Delete { old: old + trimmed },
                    GraphemeStep::Insert { new } => GraphemeStep::Insert { new: new + trimmed },
                }),
        );
        steps
    } else {
        let (n, m) = (old.len() - trimmed, new.len() - trimmed);
        let mut steps = sealed_steps(&old[..n], &new[..m]);
        steps.extend((0..trimmed).map(|index| GraphemeStep::Equal {
            old: n + index,
            new: m + index,
        }));
        steps
    }
}

fn proof_words(alphabet: &[&'static str], maximum_length: usize) -> Vec<Vec<&'static str>> {
    let mut words = Vec::new();
    for length in 0..=maximum_length {
        for mut ordinal in 0..alphabet.len().pow(length as u32) {
            let mut word = Vec::with_capacity(length);
            for _ in 0..length {
                word.push(alphabet[ordinal % alphabet.len()]);
                ordinal /= alphabet.len();
            }
            words.push(word);
        }
    }
    words
}

fn tie_proof() {
    for (old, new) in [
        ("aa", "a"),
        ("ab", "bb"),
        ("ab", "ba"),
        ("e\u{301}e\u{301}", "e\u{301}"),
    ] {
        let old = graphemes_of(old);
        let new = graphemes_of(new);
        let actual = lcs_steps(&old, &new).unwrap();
        assert_eq!(actual, sealed_steps(&old, &new));
        println!(
            "tie_vector old={old:?} new={new:?} current={actual:?} prefix={:?} suffix={:?}",
            trimmed_reference(&old, &new, true),
            trimmed_reference(&old, &new, false)
        );
    }
    for (name, words) in [
        ("binary_max6", proof_words(&["a", "b"], 6)),
        (
            "unicode_max4",
            proof_words(&["e\u{301}", "🇺🇸", "👩\u{200d}💻"], 4),
        ),
    ] {
        let mut prefix_differences = 0usize;
        for old in &words {
            for new in &words {
                let expected = sealed_steps(old, new);
                assert_eq!(
                    lcs_steps(old, new).unwrap(),
                    expected,
                    "old={old:?} new={new:?}"
                );
                assert_eq!(
                    trimmed_reference(old, new, false),
                    expected,
                    "suffix old={old:?} new={new:?}"
                );
                if trimmed_reference(old, new, true) != expected {
                    prefix_differences += 1;
                }
            }
        }
        assert!(prefix_differences > 0);
        println!(
            "tie_proof dataset={name} pairs={} current_reference=equal suffix_reference=equal prefix_differences={prefix_differences}",
            words.len() * words.len()
        );
    }
    // Public dispatch must actually use the new band: at the trailing edit
    // there is no common suffix and the full matrix exceeds the legacy cap.
    // Compare the ENTIRE script, not only distance or visible content.
    for pattern in ["a", "e\u{301}", "🇺🇸", "👩\u{200d}💻"] {
        let old = vec![pattern; 2_001];
        for (name, new) in [
            ("insert", {
                let mut new = old.clone();
                new.push("§");
                new
            }),
            ("asymmetric", {
                let mut new = old.clone();
                new.pop();
                *new.last_mut().unwrap() = "§";
                new
            }),
            ("substitute", {
                let mut new = old.clone();
                *new.last_mut().unwrap() = "§";
                new
            }),
            ("tie", {
                let mut new = old.clone();
                new[0] = "§";
                *new.last_mut().unwrap() = "§";
                new
            }),
        ] {
            assert!(!original_kernel_success(&old, &new));
            match lcs_steps(&old, &new) {
                Ok(actual) => {
                    assert_eq!(
                        actual,
                        reference_steps(&old, &new),
                        "new admission pattern={pattern:?} edit={name}"
                    );
                    println!(
                        "tie_proof dataset=public_long_band pattern={pattern:?} edit={name} current_reference=equal"
                    );
                }
                Err(InputLimitError::DiffWork) => {
                    // Component-off controls may retain the original refusal.
                    // Successful new admissions must still match the full oracle.
                    println!(
                        "tie_proof dataset=public_long_band pattern={pattern:?} edit={name} current=refusal"
                    );
                }
                Err(error) => panic!("unexpected proof error: {error}"),
            }
        }
    }
}

fn main() {
    let arguments = std::env::args().collect::<Vec<_>>();
    if arguments
        .get(1)
        .is_some_and(|argument| argument == "--alloc-probe")
    {
        let scope = arguments
            .get(2)
            .expect("allocation probe needs kernel or refresh");
        let filter = arguments.get(3).map_or("", String::as_str);
        allocation_probes(scope, filter);
    } else if arguments
        .get(1)
        .is_some_and(|argument| argument == "--tie-proof")
    {
        tie_proof();
    } else {
        benches();
        Criterion::default().configure_from_args().final_summary();
    }
}
