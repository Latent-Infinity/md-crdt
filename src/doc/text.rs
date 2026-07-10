//! Grapheme-level paragraph text as a CRDT sequence of units.

use crate::core::{OpId, PeerId, Sequence};
use unicode_segmentation::UnicodeSegmentation;

/// One grapheme cluster in a paragraph sequence.
///
/// The sequence element's `OpId` is the unit's identity for RGA and marks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextUnit {
    pub grapheme: String,
}

/// Number of grapheme clusters in `s` — the unit granularity `units_from_str` allocates.
///
/// Uses the same `graphemes(true)` segmentation so callers can predict exactly how many
/// unit OpIds an expansion of `s` will consume.
pub fn grapheme_count(s: &str) -> usize {
    s.graphemes(true).count()
}

/// Build a paragraph unit sequence from a string, allocating sequential OpIds.
///
/// Starts at `*counter` for peer `peer` and advances `counter` past the last unit.
pub fn units_from_str(s: &str, counter: &mut u64, peer: PeerId) -> Sequence<TextUnit> {
    let mut items = Vec::new();
    for g in s.graphemes(true) {
        let id = OpId {
            counter: *counter,
            peer,
        };
        *counter = counter.saturating_add(1);
        items.push((
            id,
            TextUnit {
                grapheme: g.to_string(),
            },
        ));
    }
    Sequence::from_ordered(items)
}

/// Build units starting at `start` (first unit uses `start`, then start.counter+1, …).
pub fn units_from_str_at(s: &str, start: OpId) -> Sequence<TextUnit> {
    let mut counter = start.counter;
    units_from_str(s, &mut counter, start.peer)
}

/// Visible paragraph text (skips tombstoned units).
pub fn paragraph_visible_string(seq: &Sequence<TextUnit>) -> String {
    let mut out = String::new();
    for unit in seq.iter() {
        out.push_str(&unit.grapheme);
    }
    out
}

/// Visible unit element ids in order.
pub fn paragraph_visible_ids(seq: &Sequence<TextUnit>) -> Vec<OpId> {
    seq.iter_all()
        .filter(|e| e.value.is_some())
        .map(|e| e.id)
        .collect()
}

/// Grapheme offset → left anchor for insert (`None` = start of paragraph).
pub fn after_for_grapheme_offset(seq: &Sequence<TextUnit>, grapheme_offset: usize) -> Option<OpId> {
    if grapheme_offset == 0 {
        return None;
    }
    let ids = paragraph_visible_ids(seq);
    ids.get(grapheme_offset - 1).copied()
}

/// Insert graphemes into a paragraph sequence starting at `op_id.counter`.
///
/// Returns the number of units inserted, or `None` if `grapheme_offset` is out of range.
pub fn insert_graphemes(
    seq: &mut Sequence<TextUnit>,
    grapheme_offset: usize,
    text: &str,
    op_id: OpId,
) -> Option<usize> {
    let visible_len = seq.len_visible();
    if grapheme_offset > visible_len {
        return None;
    }
    let mut after = after_for_grapheme_offset(seq, grapheme_offset);
    let mut counter = op_id.counter;
    let mut n = 0usize;
    for g in text.graphemes(true) {
        let id = OpId {
            counter,
            peer: op_id.peer,
        };
        counter = counter.saturating_add(1);
        seq.insert(
            after,
            TextUnit {
                grapheme: g.to_string(),
            },
            id,
        );
        after = Some(id);
        n += 1;
    }
    Some(n)
}
