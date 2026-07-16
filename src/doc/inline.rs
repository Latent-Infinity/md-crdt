use super::{Block, BlockKind, TextUnit, grapheme_count, paragraph_visible_ids};
use crate::core::mark::{Anchor, AnchorBias, MarkInterval, MarkKind, MarkValue};
use crate::core::{OpId, Sequence};
use std::collections::BTreeMap;

type ResolvedMark<'a> = (&'a MarkInterval, usize, usize);
type RenderKey = (MarkKind, String, String);

#[derive(Debug)]
struct ParsedMark {
    kind: MarkKind,
    start: usize,
    end: usize,
    attrs: BTreeMap<String, MarkValue>,
}

/// Parse supported inline Markdown into semantic grapheme text plus causal marks.
/// Unsupported constructs remain literal text and are still preserved exactly by source regions.
pub(super) fn parse_text_block(
    kind: impl FnOnce(Sequence<TextUnit>) -> BlockKind,
    markdown: &str,
    elem_id: OpId,
    counter: &mut u64,
) -> Block {
    let (visible, parsed_marks) = parse_fragment(markdown);
    let text = super::units_from_str(&visible, counter, 0);
    let ids = paragraph_visible_ids(&text);
    let mut block = Block::new(kind(text), elem_id);
    for parsed in parsed_marks {
        if parsed.start >= parsed.end || parsed.end > ids.len() {
            continue;
        }
        let mark_id = super::parser::next_op_id(counter);
        block.marks.set_mark(
            mark_id,
            parsed.kind,
            Anchor {
                elem_id: ids[parsed.start],
                bias: AnchorBias::Before,
            },
            Anchor {
                elem_id: ids[parsed.end - 1],
                bias: AnchorBias::After,
            },
            parsed.attrs,
            mark_id,
        );
    }
    block
}

fn parse_fragment(markdown: &str) -> (String, Vec<ParsedMark>) {
    let mut visible = String::new();
    let mut marks = Vec::new();
    let mut cursor = 0usize;
    while cursor < markdown.len() {
        let rest = &markdown[cursor..];
        if rest.starts_with("``") {
            let run = rest.chars().take_while(|ch| *ch == '`').count();
            visible.push_str(&rest[..run]);
            cursor += run;
            continue;
        }
        if let Some((open, close, kind)) = delimiter_at(rest)
            && let Some(relative_end) = find_closing_delimiter(&rest[open.len()..], close, &kind)
        {
            let inner_start = cursor + open.len();
            let inner_end = inner_start + relative_end;
            let start = grapheme_count(&visible);
            if kind == MarkKind::Code {
                visible.push_str(&markdown[inner_start..inner_end]);
            } else {
                let (inner, nested) = parse_fragment(&markdown[inner_start..inner_end]);
                visible.push_str(&inner);
                marks.extend(nested.into_iter().map(|mut mark| {
                    mark.start += start;
                    mark.end += start;
                    mark
                }));
            }
            let end = grapheme_count(&visible);
            let mut attrs = BTreeMap::new();
            attrs.insert("delimiter".into(), MarkValue::String(open.into()));
            marks.push(ParsedMark {
                kind,
                start,
                end,
                attrs,
            });
            cursor = inner_end + close.len();
            continue;
        }
        if rest.starts_with('[')
            && let Some(label_end) = rest.find("](")
            && let Some(target_end) = find_link_target_end(&rest[label_end + 2..])
        {
            let label = &rest[1..label_end];
            let target = &rest[label_end + 2..label_end + 2 + target_end];
            let start = grapheme_count(&visible);
            let (inner, mut nested) = parse_fragment(label);
            visible.push_str(&inner);
            let end = grapheme_count(&visible);
            for mark in &mut nested {
                mark.start += start;
                mark.end += start;
            }
            marks.extend(nested);
            let mut attrs = BTreeMap::new();
            attrs.insert("href".into(), MarkValue::String(target.into()));
            attrs.insert("delimiter".into(), MarkValue::String("[]()".into()));
            marks.push(ParsedMark {
                kind: MarkKind::Link,
                start,
                end,
                attrs,
            });
            cursor += label_end + 2 + target_end + 1;
            continue;
        }
        let ch = rest.chars().next().expect("cursor is in bounds");
        visible.push(ch);
        cursor += ch.len_utf8();
    }
    (visible, marks)
}

fn delimiter_at(input: &str) -> Option<(&'static str, &'static str, MarkKind)> {
    if input.starts_with("**") {
        Some(("**", "**", MarkKind::Bold))
    } else if input.starts_with('*') {
        Some(("*", "*", MarkKind::Italic))
    } else if input.starts_with('`') {
        Some(("`", "`", MarkKind::Code))
    } else {
        None
    }
}

fn find_closing_delimiter(input: &str, close: &str, kind: &MarkKind) -> Option<usize> {
    match kind {
        MarkKind::Code => find_unescaped(input, close, 0),
        MarkKind::Bold => {
            let position = find_unescaped(input, close, 0)?;
            if input[position..].starts_with("***") && has_unclosed_single_star(&input[..position])
            {
                return Some(position + 1);
            }
            Some(position)
        }
        MarkKind::Italic => {
            let mut search_from = 0;
            while let Some(position) = find_unescaped(input, "*", search_from) {
                if input[position..].starts_with("**") {
                    let nested_start = position + 2;
                    if let Some(nested_end) =
                        find_closing_delimiter(&input[nested_start..], "**", &MarkKind::Bold)
                    {
                        search_from = nested_start + nested_end + 2;
                        continue;
                    }
                }
                return Some(position);
            }
            None
        }
        MarkKind::Link | MarkKind::Custom(_) => input.find(close),
    }
}

fn find_unescaped(input: &str, needle: &str, mut search_from: usize) -> Option<usize> {
    while let Some(relative) = input[search_from..].find(needle) {
        let position = search_from + relative;
        if !is_escaped(input, position) {
            return Some(position);
        }
        search_from = position + needle.len();
    }
    None
}

fn is_escaped(input: &str, position: usize) -> bool {
    input.as_bytes()[..position]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count()
        % 2
        == 1
}

fn has_unclosed_single_star(input: &str) -> bool {
    let bytes = input.as_bytes();
    let mut index = 0;
    let mut odd_runs = 0;
    while index < bytes.len() {
        if bytes[index] != b'*' || is_escaped(input, index) {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index] == b'*' {
            index += 1;
        }
        odd_runs += (index - start) % 2;
    }
    odd_runs % 2 == 1
}

fn find_link_target_end(input: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut escaped = false;
    for (index, ch) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '(' => depth += 1,
            ')' if depth == 0 => return Some(index),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

pub(super) fn serialize_text(block: &Block, text: &Sequence<TextUnit>) -> String {
    let ids = paragraph_visible_ids(text);
    let graphemes: Vec<&str> = text.iter().map(|unit| unit.grapheme.as_str()).collect();
    let resolved = block.marks.resolved_intervals(&ids);
    if resolved.is_empty() {
        return graphemes.concat();
    }

    let mut projected = Vec::new();
    let mut link_winners: Vec<Option<&MarkInterval>> = vec![None; graphemes.len()];
    for &(interval, start, end) in &resolved {
        if start >= end || end > graphemes.len() {
            continue;
        }
        if interval.kind == MarkKind::Link {
            for winner in &mut link_winners[start..end] {
                if winner
                    .map(|current| (interval.op_id, interval.id) > (current.op_id, current.id))
                    .unwrap_or(true)
                {
                    *winner = Some(interval);
                }
            }
        } else {
            projected.push((interval, start, end));
        }
    }
    let mut index = 0;
    while index < link_winners.len() {
        let Some(winner) = link_winners[index] else {
            index += 1;
            continue;
        };
        let start = index;
        index += 1;
        while index < link_winners.len()
            && link_winners[index].is_some_and(|candidate| candidate.id == winner.id)
        {
            index += 1;
        }
        projected.push((winner, start, index));
    }

    let mut groups: BTreeMap<RenderKey, Vec<ResolvedMark<'_>>> = BTreeMap::new();
    for (interval, start, end) in projected {
        groups
            .entry((
                interval.kind.clone(),
                open_delimiter(interval),
                close_delimiter(interval),
            ))
            .or_default()
            .push((interval, start, end));
    }

    let mut intervals = Vec::new();
    for ranges in groups.values_mut() {
        ranges.sort_by_key(|(interval, start, end)| (*start, *end, interval.id));
        let Some(&(mut representative, mut start, mut end)) = ranges.first() else {
            continue;
        };
        for &(interval, next_start, next_end) in &ranges[1..] {
            if next_start <= end {
                end = end.max(next_end);
                if interval.id < representative.id {
                    representative = interval;
                }
            } else {
                intervals.push((representative, start, end));
                (representative, start, end) = (interval, next_start, next_end);
            }
        }
        intervals.push((representative, start, end));
    }

    let mut starts: Vec<Vec<_>> = vec![Vec::new(); graphemes.len() + 1];
    let mut ending_ids: Vec<Vec<_>> = vec![Vec::new(); graphemes.len() + 1];
    for (interval, start, end) in intervals {
        starts[start].push((interval, start, end));
        ending_ids[end].push(interval.id);
    }

    let mut output = String::new();
    let mut active: Vec<ResolvedMark<'_>> = Vec::new();
    let mut open_stack: Vec<&MarkInterval> = Vec::new();
    for index in 0..=graphemes.len() {
        if !ending_ids[index].is_empty() || !starts[index].is_empty() {
            active.retain(|(interval, _, _)| !ending_ids[index].contains(&interval.id));
            active.extend(starts[index].iter().copied());
            active.sort_by_key(|(interval, start, end)| {
                (
                    *start,
                    std::cmp::Reverse(*end),
                    mark_nesting_rank(&interval.kind),
                    interval.id,
                )
            });

            let desired: Vec<_> = active.iter().map(|(interval, _, _)| *interval).collect();
            let shared = open_stack
                .iter()
                .zip(&desired)
                .take_while(|(left, right)| left.id == right.id)
                .count();
            for interval in open_stack[shared..].iter().rev() {
                output.push_str(&close_delimiter(interval));
            }
            for interval in &desired[shared..] {
                output.push_str(&open_delimiter(interval));
            }
            open_stack = desired;
        }
        if let Some(grapheme) = graphemes.get(index) {
            output.push_str(grapheme);
        }
    }
    output
}

fn mark_nesting_rank(kind: &MarkKind) -> u8 {
    match kind {
        MarkKind::Link => 0,
        MarkKind::Bold => 1,
        MarkKind::Italic => 2,
        MarkKind::Code => 3,
        MarkKind::Custom(_) => 4,
    }
}

fn delimiter_attr(interval: &crate::core::mark::MarkInterval) -> Option<String> {
    interval
        .attrs
        .get("delimiter")
        .and_then(|value| match value.get_ref() {
            MarkValue::String(value) => Some(value.clone()),
            MarkValue::Bool(_) => None,
        })
}

fn open_delimiter(interval: &crate::core::mark::MarkInterval) -> String {
    match &interval.kind {
        MarkKind::Bold => delimiter_attr(interval).unwrap_or_else(|| "**".into()),
        MarkKind::Italic => delimiter_attr(interval).unwrap_or_else(|| "*".into()),
        MarkKind::Code => delimiter_attr(interval).unwrap_or_else(|| "`".into()),
        MarkKind::Link => "[".into(),
        MarkKind::Custom(_) => String::new(),
    }
}

fn close_delimiter(interval: &crate::core::mark::MarkInterval) -> String {
    match &interval.kind {
        MarkKind::Bold => delimiter_attr(interval).unwrap_or_else(|| "**".into()),
        MarkKind::Italic => delimiter_attr(interval).unwrap_or_else(|| "*".into()),
        MarkKind::Code => delimiter_attr(interval).unwrap_or_else(|| "`".into()),
        MarkKind::Link => {
            let href = interval
                .attrs
                .get("href")
                .and_then(|value| match value.get_ref() {
                    MarkValue::String(value) => Some(value.as_str()),
                    MarkValue::Bool(_) => None,
                })
                .unwrap_or_default();
            format!("]({href})")
        }
        MarkKind::Custom(_) => String::new(),
    }
}
