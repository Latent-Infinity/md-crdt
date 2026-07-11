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

/// Classic LCS dynamic program on grapheme slices (equality by string).
pub fn lcs_steps(old: &[&str], new: &[&str]) -> Vec<GraphemeStep> {
    let n = old.len();
    let m = new.len();
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            if old[i] == new[j] {
                dp[i + 1][j + 1] = dp[i][j] + 1;
            } else {
                dp[i + 1][j + 1] = dp[i + 1][j].max(dp[i][j + 1]);
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
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
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
    fn lcs_identical() {
        let a = ["a", "b", "c"];
        let steps = lcs_steps(&a, &a);
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
        let steps = lcs_steps(&old, &new);
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
    fn lcs_replace_all() {
        let old = ["a"];
        let new = ["b"];
        let steps = lcs_steps(&old, &new);
        assert!(matches!(steps[0], GraphemeStep::Delete { old: 0 }));
        assert!(matches!(steps[1], GraphemeStep::Insert { new: 0 }));
    }

    #[test]
    fn graphemes_emoji() {
        let g = graphemes_of("a🇺🇸b");
        assert_eq!(g, vec!["a", "🇺🇸", "b"]);
    }
}
