use super::{Block, BlockKind, TextUnit, grapheme_count, paragraph_visible_ids};
use crate::core::mark::{Anchor, AnchorBias, MarkKind, MarkValue};
use crate::core::{OpId, Sequence};
use std::collections::BTreeMap;

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
            && let Some(relative_end) = rest[open.len()..].find(close)
        {
            let inner_start = cursor + open.len();
            let inner_end = inner_start + relative_end;
            let start = grapheme_count(&visible);
            let (inner, nested) = parse_fragment(&markdown[inner_start..inner_end]);
            visible.push_str(&inner);
            let end = grapheme_count(&visible);
            marks.extend(nested.into_iter().map(|mut mark| {
                mark.start += start;
                mark.end += start;
                mark
            }));
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
            && let Some(target_end) = rest[label_end + 2..].find(')')
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

pub(super) fn serialize_text(block: &Block, text: &Sequence<TextUnit>) -> String {
    let ids = paragraph_visible_ids(text);
    let graphemes: Vec<&str> = text.iter().map(|unit| unit.grapheme.as_str()).collect();
    let intervals = block.marks.resolved_intervals(&ids);
    if intervals.is_empty() {
        return graphemes.concat();
    }

    let mut opens: Vec<Vec<_>> = vec![Vec::new(); graphemes.len() + 1];
    let mut closes: Vec<Vec<_>> = vec![Vec::new(); graphemes.len() + 1];
    for (interval, start, end) in intervals {
        if start < end && end <= graphemes.len() {
            opens[start].push((interval, end));
            closes[end].push((interval, start));
        }
    }
    for events in &mut opens {
        events.sort_by_key(|(interval, end)| (std::cmp::Reverse(*end), interval.id));
    }
    for events in &mut closes {
        events.sort_by_key(|(interval, start)| (std::cmp::Reverse(*start), interval.id));
    }

    let mut output = String::new();
    for index in 0..=graphemes.len() {
        for (interval, _) in &closes[index] {
            output.push_str(&close_delimiter(interval));
        }
        for (interval, _) in &opens[index] {
            output.push_str(&open_delimiter(interval));
        }
        if let Some(grapheme) = graphemes.get(index) {
            output.push_str(grapheme);
        }
    }
    output
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
