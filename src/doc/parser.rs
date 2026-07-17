use super::*;

pub struct Parser;

impl Parser {
    pub fn parse(text: &str) -> Document {
        let source_lines = source_lines(text);
        let lines: Vec<&str> = source_lines.iter().map(|line| line.text).collect();
        let mut frontmatter = None;
        let mut start_index = 0;

        if lines
            .first()
            .map(|line| line.trim() == "---")
            .unwrap_or(false)
        {
            let mut fm_lines: Vec<&str> = Vec::new();
            let mut index = 1;
            while index < lines.len() {
                if lines[index].trim() == "---" {
                    frontmatter = Some(Frontmatter::parse(fm_lines.join("\n")));
                    start_index = index + 1;
                    break;
                }
                fm_lines.push(lines[index]);
                index += 1;
            }
        }

        let mut counter = 1u64;
        let mut blocks = Vec::new();
        let mut line_spans = Vec::new();
        parse_blocks_with_spans(
            &lines[start_index..],
            &mut counter,
            &mut blocks,
            Some(&mut line_spans),
        );
        let byte_spans = line_spans
            .into_iter()
            .map(|(id, span)| {
                let start_line = start_index + span.start;
                let end_line = start_index + span.end - 1;
                (
                    id,
                    source_lines[start_line].start,
                    source_lines[end_line].content_end,
                )
            })
            .collect();
        let source = DocumentSource::new(text.to_string(), byte_spans, &blocks);

        let sequence = Sequence::from_ordered(
            blocks
                .into_iter()
                .map(|block| (block.elem_id, block))
                .collect(),
        );

        Document {
            frontmatter,
            blocks: IndexedBlocks::new(sequence),
            source: Some(source),
            block_index: RwLock::new(None),
        }
    }
}

fn parse_blocks(lines: &[&str], counter: &mut u64, out: &mut Vec<Block>) {
    parse_blocks_with_spans(lines, counter, out, None);
}

#[derive(Debug, Clone, Copy)]
struct LineSpan {
    start: usize,
    end: usize,
}

#[derive(Debug)]
struct SourceLine<'a> {
    text: &'a str,
    start: usize,
    content_end: usize,
}

fn source_lines(text: &str) -> Vec<SourceLine<'_>> {
    let mut start = 0usize;
    text.split_inclusive('\n')
        .map(|segment| {
            let full_end = start + segment.len();
            let has_newline = segment.ends_with('\n');
            let without_newline = segment.strip_suffix('\n').unwrap_or(segment);
            let logical = if has_newline {
                without_newline
                    .strip_suffix('\r')
                    .unwrap_or(without_newline)
            } else {
                without_newline
            };
            let line = SourceLine {
                text: logical,
                start,
                content_end: start + logical.len(),
            };
            start = full_end;
            line
        })
        .collect()
}

fn parse_blocks_with_spans(
    lines: &[&str],
    counter: &mut u64,
    out: &mut Vec<Block>,
    mut spans: Option<&mut Vec<(BlockId, LineSpan)>>,
) {
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            index += 1;
            continue;
        }

        if let Some((style, code_info)) = parse_fence_open(trimmed) {
            let info = code_info.trim();
            let mut contents: Vec<&str> = Vec::new();
            let mut end_index = index + 1;
            while end_index < lines.len() {
                if is_fence_close(lines[end_index].trim(), style) {
                    break;
                }
                contents.push(lines[end_index]);
                end_index += 1;
            }
            let text = contents.join("\n");
            let block = Block::new(
                BlockKind::CodeFence {
                    style,
                    info: if info.is_empty() {
                        None
                    } else {
                        Some(info.to_string())
                    },
                    text,
                },
                next_op_id(counter),
            );
            let next = (end_index + 1).min(lines.len());
            record_span(&mut spans, block.id, index, next);
            out.push(block);
            index = next;
            continue;
        }

        if trimmed.starts_with('>') {
            let mut quote_lines: Vec<&str> = Vec::new();
            let mut end_index = index;
            while end_index < lines.len() {
                let current = lines[end_index];
                let current_trimmed = current.trim();
                if !current_trimmed.starts_with('>') {
                    break;
                }
                let stripped = current_trimmed.trim_start_matches('>').trim_start();
                quote_lines.push(stripped);
                end_index += 1;
            }
            let mut child_blocks = Vec::new();
            parse_blocks(&quote_lines, counter, &mut child_blocks);
            let children = Sequence::from_ordered(
                child_blocks
                    .into_iter()
                    .map(|child| (child.elem_id, child))
                    .collect(),
            );
            let block = Block::new(BlockKind::BlockQuote { children }, next_op_id(counter));
            record_span(&mut spans, block.id, index, end_index);
            out.push(block);
            index = end_index;
            continue;
        }

        if trimmed.starts_with(":::") {
            let mut raw_lines: Vec<&str> = Vec::new();
            let mut end_index = index;
            while end_index < lines.len() {
                if lines[end_index].trim().is_empty() && end_index > index {
                    break;
                }
                raw_lines.push(lines[end_index]);
                end_index += 1;
            }
            let block = Block::new(
                BlockKind::RawBlock {
                    raw: raw_lines.join("\n"),
                },
                next_op_id(counter),
            );
            record_span(&mut spans, block.id, index, end_index);
            out.push(block);
            index = end_index;
            continue;
        }

        if index + 1 < lines.len()
            && let Some(columns) = parse_table_delimiter(lines[index + 1])
            && let Some(header) = parse_table_cells(line)
            && header.len() == columns.len()
        {
            let elem_id = next_op_id(counter);
            let mut table = Table::new(block_id_from_op(elem_id), elem_id, elem_id);
            let mut after_column = None;
            for (column, header) in columns.into_iter().zip(header) {
                let column_id = next_op_id(counter);
                table.insert_column(after_column, column.alignment, header, column_id);
                after_column = Some(column_id);
            }
            let column_ids: Vec<_> = table
                .columns_in_order()
                .into_iter()
                .map(|column| column.id)
                .collect();
            let mut after = None;
            let mut end_index = index + 2;
            while end_index < lines.len() {
                let Some(cells) = parse_table_cells(lines[end_index]) else {
                    break;
                };
                let row_id = next_op_id(counter);
                table.insert_row(
                    after,
                    column_ids.iter().copied().zip(cells).collect(),
                    row_id,
                );
                after = Some(row_id);
                end_index += 1;
            }
            let block = Block::new(
                BlockKind::Table {
                    table: Box::new(table),
                },
                elem_id,
            );
            record_span(&mut spans, block.id, index, end_index);
            out.push(block);
            index = end_index;
            continue;
        }

        // ATX heading: # .. ######
        if let Some((level, title)) = parse_atx_heading(trimmed) {
            let elem_id = next_op_id(counter);
            let block = inline::parse_text_block(
                |text| BlockKind::Heading { level, text },
                title,
                elem_id,
                counter,
            );
            record_span(&mut spans, block.id, index, index + 1);
            out.push(block);
            index += 1;
            continue;
        }

        // Unordered / ordered list
        if is_list_start(trimmed) {
            let (list_block, next) = parse_list(lines, index, counter, indent_of(line));
            record_span(&mut spans, list_block.id, index, next);
            out.push(list_block);
            index = next;
            continue;
        }

        // Setext heading: title line + === or ---
        if index + 1 < lines.len()
            && let Some(level) = parse_setext_underline(lines[index + 1].trim())
        {
            let title = trimmed;
            let elem_id = next_op_id(counter);
            let block = inline::parse_text_block(
                |text| BlockKind::Heading { level, text },
                title,
                elem_id,
                counter,
            );
            record_span(&mut spans, block.id, index, index + 2);
            out.push(block);
            index += 2;
            continue;
        }

        let mut paragraph_lines: Vec<&str> = Vec::new();
        let mut end_index = index;
        while end_index < lines.len() {
            let current = lines[end_index];
            let current_trimmed = current.trim();
            if current_trimmed.is_empty()
                || parse_fence_open(current_trimmed).is_some()
                || current_trimmed.starts_with('>')
                || current_trimmed.starts_with(":::")
                || parse_atx_heading(current_trimmed).is_some()
                || is_list_start(current_trimmed)
            {
                break;
            }
            // Stop before a setext underline that would apply to a single prior line only
            // (already handled above when starting a new block).
            paragraph_lines.push(current);
            end_index += 1;
        }
        let elem_id = next_op_id(counter);
        let block = inline::parse_text_block(
            |text| BlockKind::Paragraph { text },
            &paragraph_lines.join("\n"),
            elem_id,
            counter,
        );
        record_span(&mut spans, block.id, index, end_index);
        out.push(block);
        index = end_index;
    }
}

fn record_span(
    spans: &mut Option<&mut Vec<(BlockId, LineSpan)>>,
    id: BlockId,
    start: usize,
    end: usize,
) {
    if let Some(spans) = spans.as_deref_mut() {
        spans.push((id, LineSpan { start, end }));
    }
}

fn parse_table_cells(line: &str) -> Option<Vec<CellContent>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let trailing_backslashes = inner
        .as_bytes()
        .iter()
        .rev()
        .skip(1)
        .take_while(|byte| **byte == b'\\')
        .count();
    let trailing_pipe_is_delimiter = inner.ends_with('|') && trailing_backslashes % 2 == 0;
    let inner = if trailing_pipe_is_delimiter {
        &inner[..inner.len() - 1]
    } else {
        inner
    };
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut characters = inner.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '|' => {
                cells.push(cell.trim().to_string());
                cell.clear();
            }
            '\\' => match characters.peek().copied() {
                Some('|' | '\\') => {
                    cell.push(characters.next().expect("peeked escaped table character"));
                }
                _ => cell.push('\\'),
            },
            _ => cell.push(character),
        }
    }
    cells.push(cell.trim().to_string());
    (!cells.is_empty()).then_some(cells)
}

fn parse_table_delimiter(line: &str) -> Option<Vec<ColumnDef>> {
    let cells = parse_table_cells(line)?;
    cells
        .into_iter()
        .map(|cell| {
            let marker = cell.trim();
            let left = marker.starts_with(':');
            let right = marker.ends_with(':');
            let dashes = marker.trim_matches(':');
            if dashes.len() < 3 || !dashes.chars().all(|c| c == '-') {
                return None;
            }
            Some(ColumnDef {
                alignment: match (left, right) {
                    (true, true) => ColumnAlignment::Center,
                    (_, true) => ColumnAlignment::Right,
                    _ => ColumnAlignment::Left,
                },
            })
        })
        .collect()
}

fn indent_of(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .fold(0, |column, c| {
            if c == '\t' {
                column + (4 - column % 4)
            } else {
                column + 1
            }
        })
}

fn parse_atx_heading(trimmed: &str) -> Option<(u8, &str)> {
    let bytes = trimmed.as_bytes();
    let mut level = 0u8;
    while (level as usize) < bytes.len() && bytes[level as usize] == b'#' && level < 6 {
        level += 1;
    }
    if level == 0 {
        return None;
    }
    // Must not be a 7th #
    if (level as usize) < bytes.len() && bytes[level as usize] == b'#' {
        return None;
    }
    let suffix = &trimmed[level as usize..];
    if !suffix.is_empty() && !suffix.starts_with([' ', '\t']) {
        return None;
    }
    let rest = suffix.trim_start();
    let rest = strip_atx_closing_sequence(rest);
    Some((level, rest))
}

fn strip_atx_closing_sequence(text: &str) -> &str {
    let trimmed = text.trim_end();
    let closing_start = trimmed.trim_end_matches('#').len();
    if closing_start == trimmed.len() {
        return trimmed;
    }
    let before = &trimmed[..closing_start];
    if before.is_empty() || before.ends_with([' ', '\t']) {
        before.trim_end()
    } else {
        trimmed
    }
}

fn parse_setext_underline(trimmed: &str) -> Option<u8> {
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().all(|c| c == '=') {
        return Some(1);
    }
    if trimmed.chars().all(|c| c == '-') {
        return Some(2);
    }
    None
}

fn is_list_start(trimmed: &str) -> bool {
    unordered_marker(trimmed).is_some() || ordered_marker(trimmed).is_some()
}

fn unordered_marker(trimmed: &str) -> Option<(BulletMarker, &str)> {
    for (marker, symbol) in [
        (BulletMarker::Dash, '-'),
        (BulletMarker::Asterisk, '*'),
        (BulletMarker::Plus, '+'),
    ] {
        let Some(rest) = trimmed.strip_prefix(symbol) else {
            continue;
        };
        if rest.is_empty() {
            return Some((marker, ""));
        }
        if let Some(rest) = rest.strip_prefix([' ', '\t']) {
            return Some((marker, rest.trim_start()));
        }
    }
    None
}

fn ordered_marker(trimmed: &str) -> Option<(u32, ListDelimiter, &str)> {
    let mut i = 0usize;
    let bytes = trimmed.as_bytes();
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 || i > 9 {
        return None;
    }
    let num: u32 = trimmed[..i].parse().ok()?;
    let rest = &trimmed[i..];
    for (delimiter, symbol) in [
        (ListDelimiter::Period, '.'),
        (ListDelimiter::Parenthesis, ')'),
    ] {
        let Some(tail) = rest.strip_prefix(symbol) else {
            continue;
        };
        if tail.is_empty() {
            return Some((num, delimiter, ""));
        }
        if let Some(body) = tail.strip_prefix([' ', '\t']) {
            return Some((num, delimiter, body.trim_start()));
        }
    }
    None
}

fn parse_fence_open(trimmed: &str) -> Option<(CodeFenceStyle, &str)> {
    let marker = match trimmed.chars().next()? {
        '`' => FenceMarker::Backtick,
        '~' => FenceMarker::Tilde,
        _ => return None,
    };
    let symbol = match marker {
        FenceMarker::Backtick => '`',
        FenceMarker::Tilde => '~',
    };
    let length = trimmed
        .chars()
        .take_while(|character| *character == symbol)
        .count();
    if length < 3 || length > u8::MAX as usize {
        return None;
    }
    let rest = &trimmed[length..];
    if marker == FenceMarker::Backtick && rest.contains('`') {
        return None;
    }
    Some((
        CodeFenceStyle {
            marker,
            length: length as u8,
        },
        rest,
    ))
}

fn is_fence_close(trimmed: &str, style: CodeFenceStyle) -> bool {
    let symbol = match style.marker {
        FenceMarker::Backtick => '`',
        FenceMarker::Tilde => '~',
    };
    let length = trimmed
        .chars()
        .take_while(|character| *character == symbol)
        .count();
    length >= usize::from(style.length) && trimmed[length..].chars().all(char::is_whitespace)
}

fn push_list_paragraph(children: &mut Vec<Block>, lines: &mut Vec<&str>, counter: &mut u64) {
    if lines.is_empty() {
        return;
    }
    let joined = lines.join("\n");
    lines.clear();
    let elem_id = next_op_id(counter);
    children.push(inline::parse_text_block(
        |text| BlockKind::Paragraph { text },
        &joined,
        elem_id,
        counter,
    ));
}

/// Parse a list starting at `index` with items at `base_indent` or greater content indent.
fn parse_list(
    lines: &[&str],
    index: usize,
    counter: &mut u64,
    base_indent: usize,
) -> (Block, usize) {
    let first_trim = lines[index].trim();
    let style = if let Some((start, delimiter, _)) = ordered_marker(first_trim) {
        ListStyle {
            ordered: true,
            start,
            delimiter,
            ..ListStyle::default()
        }
    } else {
        ListStyle {
            bullet: unordered_marker(first_trim)
                .map(|(marker, _)| marker)
                .unwrap_or(BulletMarker::Dash),
            ..ListStyle::default()
        }
    };
    let mut style = style;
    let mut items: Vec<ListItem> = Vec::new();
    let mut i = index;

    while i < lines.len() {
        let line = lines[i];
        let ind = indent_of(line);
        if ind < base_indent && !line.trim().is_empty() {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            // Blank line inside list: peek if more list content follows
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim().is_empty() {
                j += 1;
            }
            if j >= lines.len() {
                break;
            }
            let next_ind = indent_of(lines[j]);
            let next_trim = lines[j].trim();
            let same_list = if style.ordered {
                ordered_marker(next_trim)
                    .is_some_and(|(_, delimiter, _)| delimiter == style.delimiter)
            } else {
                unordered_marker(next_trim).is_some_and(|(marker, _)| marker == style.bullet)
            };
            if next_ind > base_indent || (next_ind == base_indent && same_list) {
                style.loose = true;
                i += 1;
                continue;
            }
            break;
        }

        let marker_body = if style.ordered {
            ordered_marker(trimmed)
                .filter(|(_, delimiter, _)| *delimiter == style.delimiter)
                .map(|(_, _, body)| body)
        } else {
            unordered_marker(trimmed)
                .filter(|(marker, _)| *marker == style.bullet)
                .map(|(_, body)| body)
        };
        let Some(body) = marker_body else {
            break;
        };
        if ind > base_indent {
            // Nested list belongs to previous item
            break;
        }
        if ind < base_indent {
            break;
        }

        let item_elem = next_op_id(counter);
        let (task, body) = parse_task_marker(body);
        let mut children = Vec::new();
        // First paragraph of the item
        let mut para_lines = if body.is_empty() {
            Vec::new()
        } else {
            vec![body]
        };
        i += 1;

        // Continuation lines for this item
        while i < lines.len() {
            let cl = lines[i];
            let cind = indent_of(cl);
            let ctrim = cl.trim();
            if ctrim.is_empty() {
                // Look ahead for continuation or nested list
                let mut j = i + 1;
                while j < lines.len() && lines[j].trim().is_empty() {
                    j += 1;
                }
                if j < lines.len() && indent_of(lines[j]) > base_indent {
                    style.loose = true;
                    if !is_list_start(lines[j].trim()) {
                        push_list_paragraph(&mut children, &mut para_lines, counter);
                    }
                    i += 1;
                    continue;
                }
                break;
            }
            if cind > base_indent && is_list_start(ctrim) {
                // Nested list
                push_list_paragraph(&mut children, &mut para_lines, counter);
                let (nested, next) = parse_list(lines, i, counter, cind);
                children.push(nested);
                i = next;
                continue;
            }
            if cind > base_indent {
                // Continued paragraph in item (indented)
                para_lines.push(ctrim);
                i += 1;
                continue;
            }
            // Same indent: new list item or end
            break;
        }

        push_list_paragraph(&mut children, &mut para_lines, counter);

        let child_seq =
            Sequence::from_ordered(children.into_iter().map(|b| (b.elem_id, b)).collect());
        items.push(ListItem {
            id: block_id_from_op(item_elem),
            elem_id: item_elem,
            task,
            task_op: item_elem,
            task_observed: StateVector::new(),
            placement_observed: StateVector::new(),
            children: child_seq,
        });
    }

    let list_elem = next_op_id(counter);
    let items_seq = Sequence::from_ordered(items.into_iter().map(|it| (it.elem_id, it)).collect());
    let block = Block::new(
        BlockKind::List {
            style,
            items: items_seq,
            pending_moves: Vec::new(),
        },
        list_elem,
    );
    (block, i)
}

fn parse_task_marker(body: &str) -> (Option<TaskState>, &str) {
    for (prefix, state) in [
        ("[ ]", TaskState::Unchecked),
        ("[x]", TaskState::Checked),
        ("[X]", TaskState::Checked),
    ] {
        if let Some(rest) = body.strip_prefix(prefix)
            && (rest.is_empty() || rest.starts_with([' ', '\t']))
        {
            return (Some(state), rest.trim_start());
        }
    }
    (None, body)
}

pub(super) fn next_op_id(counter: &mut u64) -> OpId {
    let id = OpId {
        counter: *counter,
        peer: 0,
    };
    *counter += 1;
    id
}
