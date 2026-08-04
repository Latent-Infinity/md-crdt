//! Shared size matrix and ASCII fixture constants for controlled scenarios.

use crate::{PEER_A, PEER_B};

/// Visible text lengths used by v1 workloads.
pub const TEXT_LENS: &[usize] = &[1_000, 10_000];

/// Append run / keystroke batch lengths.
pub const APPEND_LENS: &[usize] = &[32, 256];

/// Delta lag (one-byte edits) counts for history=delta cases.
pub const DELTA_LAGS: &[usize] = &[1, 100];

/// Single-call paste lengths (stretch workloads).
pub const PASTE_LENS: &[usize] = &[256, 1_024, 4_096];

/// Peer counts for multi-peer fan-in stretch (includes the sink peer).
pub const FANIN_PEER_COUNTS: &[usize] = &[4, 8];

/// Base text length for fan-in stretch cases.
pub const FANIN_BASE_N: usize = 1_000;

/// Seed fill codepoint (ASCII so grapheme, scalar, UTF-16 unit, and byte offsets coincide).
pub const FILL_BYTE: u8 = b'x';

/// Single-unit middle insert payload for sequential insert scenarios.
pub const MIDDLE_INSERT: &str = "y";

/// One-byte keystroke / lag-edit payload byte.
pub const KEYSTROKE_BYTE: u8 = b'z';

/// One-byte keystroke / lag-edit payload as `&str`.
pub const KEYSTROKE_PAYLOAD: &str = "z";

/// Concurrent markers for the two-peer round-trip schedule.
pub const PEER_A_MARKER: &str = "a";
pub const PEER_B_MARKER: &str = "b";

/// Canonical size matrix referenced by scenarios and benches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeMatrix {
    pub text_lens: &'static [usize],
    pub append_lens: &'static [usize],
    pub delta_lags: &'static [usize],
}

/// Default v1 matrix from the comparison plan.
pub const V1_SIZE_MATRIX: SizeMatrix = SizeMatrix {
    text_lens: TEXT_LENS,
    append_lens: APPEND_LENS,
    delta_lags: DELTA_LAGS,
};

/// Peer ids fixed by methodology (also exported from the crate root).
#[must_use]
pub const fn peer_a() -> u64 {
    PEER_A
}

/// Peer ids fixed by methodology (also exported from the crate root).
#[must_use]
pub const fn peer_b() -> u64 {
    PEER_B
}

/// Build the seed body of `n` fill bytes as a `String`.
#[must_use]
pub fn fill_text(n: usize) -> String {
    String::from_utf8(vec![FILL_BYTE; n]).expect("FILL_BYTE is valid ASCII")
}

/// Build an M-byte ASCII append run (repeated KEYSTROKE_BYTE).
#[must_use]
pub fn append_run_payload(m: usize) -> String {
    String::from_utf8(vec![KEYSTROKE_BYTE; m]).expect("KEYSTROKE_BYTE is valid ASCII")
}

/// Build an R-byte ASCII paste payload (repeated `'p'`).
#[must_use]
pub fn paste_payload(r: usize) -> String {
    String::from_utf8(vec![b'p'; r]).expect("paste byte is valid ASCII")
}

/// Unique single-byte marker for peer `peer` (1..=26 → A..Z).
#[must_use]
pub fn peer_marker(peer: u64) -> String {
    assert!((1..=26).contains(&peer), "peer marker range 1..=26");
    let ch = b'A' + u8::try_from(peer - 1).expect("peer fits u8");
    String::from_utf8(vec![ch]).expect("ASCII marker")
}

/// Single keystroke payload as `&str`.
#[must_use]
pub const fn keystroke_payload() -> &'static str {
    KEYSTROKE_PAYLOAD
}

/// Middle index for inserts/deletes on a length-`n` body (`n / 2`).
#[must_use]
pub const fn middle_index(n: usize) -> usize {
    n / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_matrix_matches_plan() {
        assert_eq!(TEXT_LENS, &[1_000, 10_000]);
        assert_eq!(APPEND_LENS, &[32, 256]);
        assert_eq!(DELTA_LAGS, &[1, 100]);
        assert_eq!(V1_SIZE_MATRIX.text_lens, TEXT_LENS);
        assert_eq!(V1_SIZE_MATRIX.append_lens, APPEND_LENS);
        assert_eq!(V1_SIZE_MATRIX.delta_lags, DELTA_LAGS);
    }

    #[test]
    fn peer_ids_are_fixed() {
        assert_eq!(peer_a(), 1);
        assert_eq!(peer_b(), 2);
        assert_ne!(peer_a(), peer_b());
    }

    #[test]
    fn ascii_fillers_are_single_byte() {
        assert_eq!(FILL_BYTE, b'x');
        assert_eq!(MIDDLE_INSERT, "y");
        assert_eq!(MIDDLE_INSERT.len(), 1);
        assert_eq!(KEYSTROKE_BYTE, b'z');
        assert_eq!(keystroke_payload(), "z");
        assert_eq!(keystroke_payload().len(), 1);
        assert_eq!(PEER_A_MARKER, "a");
        assert_eq!(PEER_B_MARKER, "b");
    }

    #[test]
    fn fill_and_append_lengths() {
        assert_eq!(fill_text(0), "");
        assert_eq!(fill_text(4), "xxxx");
        assert_eq!(fill_text(1_000).len(), 1_000);
        assert_eq!(append_run_payload(32).len(), 32);
        assert!(append_run_payload(256).chars().all(|c| c == 'z'));
    }

    #[test]
    fn middle_index_is_half_floor() {
        assert_eq!(middle_index(1_000), 500);
        assert_eq!(middle_index(10_000), 5_000);
        assert_eq!(middle_index(5), 2);
    }

    #[test]
    fn stretch_sizes_and_markers() {
        assert_eq!(PASTE_LENS, &[256, 1_024, 4_096]);
        assert_eq!(FANIN_PEER_COUNTS, &[4, 8]);
        assert_eq!(paste_payload(3), "ppp");
        assert_eq!(peer_marker(1), "A");
        assert_eq!(peer_marker(2), "B");
        assert_eq!(peer_marker(8), "H");
    }
}
