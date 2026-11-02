//! Grapheme-level LCS for vault text ingest (D2).
//!
//! Given an existing paragraph's visible units and a desired grapheme sequence,
//! produce delete/insert steps that preserve OpIds for the LCS.

use unicode_segmentation::UnicodeSegmentation;

/// One step of a grapheme LCS alignment (old index ↔ new index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphemeStep {
    /// Grapheme present in both; keep the old unit OpId.
    Equal { old: usize, new: usize },
    /// Drop the old unit at `old`.
    Delete { old: usize },
    /// Insert the new grapheme at `new` (after previous retained/inserted material).
    Insert { new: usize },
}

const MATRIX_LIMIT: usize = 4_000_000;

/// Exact backward-equal-first LCS alignment with bounded local-edit admission.
pub fn lcs_steps(
    old: &[&str],
    new: &[&str],
) -> Result<Vec<GraphemeStep>, crate::limits::InputLimitError> {
    let cells = old
        .len()
        .saturating_add(1)
        .saturating_mul(new.len().saturating_add(1));
    let output = old.len().saturating_add(new.len());
    let scan = old.len().min(new.len()).saturating_add(1);
    // Preserve every legacy admission. Choose this BEFORE scanning: discovery
    // must never consume headroom needed by an originally admitted dense table.
    if cells <= MATRIX_LIMIT && !WorkBudget::new().fits_table(cells, scan, output) {
        return admitted_dense_steps(old, new);
    }
    // Each individual dimension was bounded by the old matrix admission too.
    if old.len() >= MATRIX_LIMIT || new.len() >= MATRIX_LIMIT {
        return Err(crate::limits::InputLimitError::DiffWork);
    }
    let mut budget = WorkBudget::new();
    // Reserve all output work/bytes before discovery, including allocation,
    // traceback, suffix stitching and reversal. No per-round budget resets.
    budget.charge(
        output.saturating_mul(3),
        output.saturating_mul(size_of::<GraphemeStep>()),
    )?;
    let mut suffix = 0;
    while suffix < old.len().min(new.len()) {
        budget.charge(1, 0)?;
        if old[old.len() - suffix - 1] != new[new.len() - suffix - 1] {
            break;
        }
        suffix += 1;
    }
    reduced_steps(old, new, suffix, &mut budget)
}

struct WorkBudget {
    work: usize,
    bytes: usize,
}

impl WorkBudget {
    fn new() -> Self {
        Self {
            work: MATRIX_LIMIT,
            bytes: MATRIX_LIMIT * size_of::<u32>(),
        }
    }

    fn charge(&mut self, work: usize, bytes: usize) -> Result<(), crate::limits::InputLimitError> {
        if work > self.work || bytes > self.bytes {
            return Err(crate::limits::InputLimitError::DiffWork);
        }
        self.work -= work;
        self.bytes -= bytes;
        Ok(())
    }

    fn fits_table(&self, cells: usize, scan: usize, output: usize) -> bool {
        // Five units per slot cover allocation, possible growth-copy,
        // initialization, recurrence and row/round administration.
        cells
            .saturating_mul(5)
            .saturating_add(scan)
            .saturating_add(output.saturating_mul(3))
            <= self.work
            && cells
                .saturating_mul(size_of::<u32>())
                .saturating_add(output.saturating_mul(size_of::<GraphemeStep>()))
                <= self.bytes
    }
}

fn zero_table(
    cells: usize,
    table: &mut Vec<u32>,
    budget: &mut WorkBudget,
) -> Result<(), crate::limits::InputLimitError> {
    budget.charge(
        cells.saturating_mul(5),
        cells.saturating_mul(size_of::<u32>()),
    )?;
    if cells > table.len() {
        table
            .try_reserve_exact(cells - table.len())
            .map_err(|_| crate::limits::InputLimitError::DiffWork)?;
    }
    table.clear();
    table.resize(cells, 0);
    Ok(())
}

fn dense_table(
    old: &[&str],
    new: &[&str],
    budget: &mut WorkBudget,
) -> Result<Vec<u32>, crate::limits::InputLimitError> {
    let mut dp = Vec::new();
    zero_table(
        (old.len() + 1).saturating_mul(new.len() + 1),
        &mut dp,
        budget,
    )?;
    fill_dense(old, new, &mut dp);
    Ok(dp)
}

fn fill_dense(old: &[&str], new: &[&str], dp: &mut [u32]) {
    let stride = new.len() + 1;
    for i in 0..old.len() {
        for j in 0..new.len() {
            dp[(i + 1) * stride + j + 1] = if old[i] == new[j] {
                dp[i * stride + j] + 1
            } else {
                dp[i * stride + j + 1].max(dp[(i + 1) * stride + j])
            };
        }
    }
}

fn band_score(dp: &[u32], stride: usize, width: usize, i: usize, j: usize) -> u32 {
    if i.abs_diff(j) > width {
        0
    } else {
        dp[i * stride + j + width - i]
    }
}

fn banded_table(
    old: &[&str],
    new: &[&str],
    budget: &mut WorkBudget,
) -> Result<(Vec<u32>, usize), crate::limits::InputLimitError> {
    let (n, m) = (old.len(), new.len());
    let mut width = n.abs_diff(m).max(1);
    let mut dp = Vec::new();
    loop {
        let stride = width.saturating_mul(2).saturating_add(1);
        zero_table((n + 1).saturating_mul(stride), &mut dp, budget)?;
        for i in 1..=n {
            for j in i.saturating_sub(width).max(1)..=m.min(i.saturating_add(width)) {
                dp[i * stride + j + width - i] = if old[i - 1] == new[j - 1] {
                    band_score(&dp, stride, width, i - 1, j - 1) + 1
                } else {
                    band_score(&dp, stride, width, i - 1, j).max(band_score(
                        &dp,
                        stride,
                        width,
                        i,
                        j - 1,
                    ))
                };
            }
        }
        let distance = n + m - 2 * band_score(&dp, stride, width, n, m) as usize;
        // A stored score is a real subsequence, so distance is an upper bound.
        // When distance <= width, every globally optimal edit path stays in the
        // band (diagonal displacement <= total edits). Thus all optimal choices,
        // including tied predecessors, are present. Lower off-path scores cannot
        // overturn the original equality-first / insertion-on-tie traceback.
        if distance <= width {
            return Ok((dp, width));
        }
        // Reuse the flat buffer, but failed rounds retain all their charges.
        // No uncharged dense fallback after failed discovery.
        width = width.saturating_mul(2);
    }
}

fn reduced_steps(
    old: &[&str],
    new: &[&str],
    suffix: usize,
    budget: &mut WorkBudget,
) -> Result<Vec<GraphemeStep>, crate::limits::InputLimitError> {
    let (n, m) = (old.len() - suffix, new.len() - suffix);
    let cells = (n + 1).saturating_mul(m + 1);
    let (dp, width) = if n == 0 || m == 0 {
        (Vec::new(), None)
    } else if budget.fits_table(cells, 0, 0) {
        (dense_table(&old[..n], &new[..m], budget)?, None)
    } else {
        let (dp, width) = banded_table(&old[..n], &new[..m], budget)?;
        (dp, Some(width))
    };
    trace_steps(old, new, suffix, &dp, width)
}

fn trace_steps(
    old: &[&str],
    new: &[&str],
    suffix: usize,
    dp: &[u32],
    width: Option<usize>,
) -> Result<Vec<GraphemeStep>, crate::limits::InputLimitError> {
    let (n, m) = (old.len() - suffix, new.len() - suffix);
    let score = |i, j| match width {
        Some(width) => band_score(dp, 2 * width + 1, width, i, j),
        None => dp[i * (m + 1) + j],
    };
    let residual_lcs = if n == 0 || m == 0 {
        0
    } else {
        score(n, m) as usize
    };
    let output_len = n + m - residual_lcs + suffix;
    let mut steps = Vec::new();
    // Optimized callers prepay old+new; legacy admissions keep their old limits.
    steps
        .try_reserve_exact(output_len)
        .map_err(|_| crate::limits::InputLimitError::DiffWork)?;
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old[i - 1] == new[j - 1] {
            steps.push(GraphemeStep::Equal {
                old: i - 1,
                new: j - 1,
            });
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || (i > 0 && score(i, j - 1) >= score(i - 1, j))) {
            steps.push(GraphemeStep::Insert { new: j - 1 });
            j -= 1;
        } else {
            steps.push(GraphemeStep::Delete { old: i - 1 });
            i -= 1;
        }
    }
    steps.reverse();
    // These pairs are precisely the first forced choices in backward traceback;
    // trimming an equal prefix instead would change aa -> a's retained identity.
    steps.extend((0..suffix).map(|index| GraphemeStep::Equal {
        old: n + index,
        new: m + index,
    }));
    Ok(steps)
}

// Original matrix admission is grandfathered before discovery: do not apply
// the stricter optimized work budget or narrow its admitted input set.
fn admitted_dense_steps(
    old: &[&str],
    new: &[&str],
) -> Result<Vec<GraphemeStep>, crate::limits::InputLimitError> {
    let mut dp = vec![0u32; (old.len() + 1) * (new.len() + 1)];
    fill_dense(old, new, &mut dp);
    trace_steps(old, new, 0, &dp, None)
}

// Frozen old recurrence AND traceback: independent proof reference only.
#[cfg(test)]
fn sealed_reference_steps(old: &[&str], new: &[&str]) -> Vec<GraphemeStep> {
    let n = old.len();
    let m = new.len();
    let stride = m + 1;
    let mut dp = vec![0u32; (n + 1) * stride];
    for i in 0..n {
        for j in 0..m {
            if old[i] == new[j] {
                dp[(i + 1) * stride + j + 1] = dp[i * stride + j] + 1;
            } else {
                dp[(i + 1) * stride + j + 1] = dp[i * stride + j + 1].max(dp[(i + 1) * stride + j]);
            }
        }
    }

    let mut steps = Vec::new();
    let mut i = n;
    let mut j = m;
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

/// Split `s` into grapheme clusters (same segmentation as paragraph units).
pub fn graphemes_of(s: &str) -> Vec<&str> {
    s.graphemes(true).collect()
}

/// Indices of old units that must be tombstoned (not in LCS), high→low for safe offset deletes.
pub fn delete_indices_high_to_low(steps: &[GraphemeStep]) -> Vec<usize> {
    let mut v: Vec<usize> = steps
        .iter()
        .filter_map(|s| match s {
            GraphemeStep::Delete { old } => Some(*old),
            _ => None,
        })
        .collect();
    v.sort_unstable();
    v.reverse();
    v
}

/// New grapheme indices that need insert (not in LCS), in left-to-right order.
pub fn insert_new_indices(steps: &[GraphemeStep]) -> Vec<usize> {
    steps
        .iter()
        .filter_map(|s| match s {
            GraphemeStep::Insert { new } => Some(*new),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dissimilar_oversized_alignment_is_bounded() {
        let old = vec!["x"; 5_000];
        let new = vec!["y"; 5_000];
        assert_eq!(
            lcs_steps(&old, &new),
            Err(crate::limits::InputLimitError::DiffWork)
        );
    }

    fn words(alphabet: &[&'static str], maximum: usize) -> Vec<Vec<&'static str>> {
        let mut words = Vec::new();
        for length in 0..=maximum {
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

    #[test]
    fn certified_band_matches_entire_legacy_vector_exhaustively() {
        // Call the actual band, not public small-input dense dispatch. Includes
        // empty/asymmetric inputs, ties, repeated clusters and widening rounds.
        for words in [
            words(&["a", "b"], 6),
            words(&["e\u{301}", "🇺🇸", "👩\u{200d}💻"], 4),
        ] {
            for old in &words {
                for new in &words {
                    let mut budget = WorkBudget::new();
                    let output = old.len() + new.len();
                    budget
                        .charge(output * 3, output * size_of::<GraphemeStep>())
                        .unwrap();
                    let (table, width) = banded_table(old, new, &mut budget).unwrap();
                    let actual = trace_steps(old, new, 0, &table, Some(width)).unwrap();
                    assert_eq!(
                        actual,
                        sealed_reference_steps(old, new),
                        "old={old:?} new={new:?} width={width}"
                    );
                    assert_eq!(lcs_steps(old, new).unwrap(), actual);
                }
            }
        }
    }

    #[test]
    fn long_local_edits_keep_exact_legacy_correspondence() {
        for pattern in ["a", "e\u{301}", "🇺🇸", "👩\u{200d}💻"] {
            let old = vec![pattern; 2_001];
            for position in [0, 1_000, 2_000] {
                for edit in ["insert", "delete", "substitute"] {
                    let mut new = old.clone();
                    match edit {
                        "insert" => new.insert(position, "§"),
                        "delete" => {
                            new.remove(position);
                        }
                        "substitute" => new[position] = "§",
                        _ => unreachable!(),
                    }
                    assert_eq!(
                        lcs_steps(&old, &new).unwrap(),
                        sealed_reference_steps(&old, &new)
                    );
                }
            }
        }
    }

    #[test]
    fn legacy_dense_boundary_and_asymmetric_admissions_remain_supported() {
        for (n, m) in [(1_999, 1_999), (0, 3_999), (3_999, 0)] {
            let old = vec!["a"; n];
            let new = vec!["b"; m];
            let steps = lcs_steps(&old, &new).unwrap();
            assert_eq!(steps.len(), n + m);
            assert!(
                steps[..n]
                    .iter()
                    .enumerate()
                    .all(|(old, step)| *step == GraphemeStep::Delete { old })
            );
            assert!(
                steps[n..]
                    .iter()
                    .enumerate()
                    .all(|(new, step)| *step == GraphemeStep::Insert { new })
            );
        }
    }

    #[test]
    fn lcs_identical() {
        let a = ["a", "b", "c"];
        let steps = lcs_steps(&a, &a).unwrap();
        assert!(
            steps
                .iter()
                .all(|s| matches!(s, GraphemeStep::Equal { .. }))
        );
        assert_eq!(steps.len(), 3);
    }

    #[test]
    fn lcs_insert_middle() {
        let old = ["a", "b"];
        let new = ["a", "x", "b"];
        let steps = lcs_steps(&old, &new).unwrap();
        assert_eq!(
            steps,
            vec![
                GraphemeStep::Equal { old: 0, new: 0 },
                GraphemeStep::Insert { new: 1 },
                GraphemeStep::Equal { old: 1, new: 2 },
            ]
        );
    }

    #[test]
    fn repeated_graphemes_preserve_backward_traceback_identity_choices() {
        for (old, new, expected) in [
            (
                "aa",
                "a",
                vec![
                    GraphemeStep::Delete { old: 0 },
                    GraphemeStep::Equal { old: 1, new: 0 },
                ],
            ),
            (
                "ab",
                "bb",
                vec![
                    GraphemeStep::Delete { old: 0 },
                    GraphemeStep::Insert { new: 0 },
                    GraphemeStep::Equal { old: 1, new: 1 },
                ],
            ),
            (
                "ab",
                "ba",
                vec![
                    GraphemeStep::Delete { old: 0 },
                    GraphemeStep::Equal { old: 1, new: 0 },
                    GraphemeStep::Insert { new: 1 },
                ],
            ),
        ] {
            assert_eq!(
                lcs_steps(&graphemes_of(old), &graphemes_of(new)).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn lcs_replace_all() {
        let old = ["a"];
        let new = ["b"];
        let steps = lcs_steps(&old, &new).unwrap();
        assert!(matches!(steps[0], GraphemeStep::Delete { old: 0 }));
        assert!(matches!(steps[1], GraphemeStep::Insert { new: 0 }));
    }

    #[test]
    fn graphemes_emoji() {
        let g = graphemes_of("a🇺🇸b");
        assert_eq!(g, vec!["a", "🇺🇸", "b"]);
    }
}
