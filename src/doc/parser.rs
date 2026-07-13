use super::*;

pub struct Parser;

impl Parser {
    pub fn parse(text: &str) -> Document {
        let lines: Vec<&str> = text.lines().collect();
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
                    frontmatter = Some(fm_lines.join("\n"));
                    start_index = index + 1;
                    break;
                }
                fm_lines.push(lines[index]);
                index += 1;
            }
        }

        let mut counter = 1u64;
        let mut blocks = Vec::new();
        parse_blocks(&lines[start_index..], &mut counter, &mut blocks);

        let sequence = Sequence::from_ordered(
            blocks
                .into_iter()
                .map(|block| (block.elem_id, block))
                .collect(),
        );

        Document {
            frontmatter,
            blocks: IndexedBlocks::new(sequence),
            raw_source: Some(text.to_string()),
            block_index: RwLock::new(None),
        }
    }
}

fn parse_blocks(lines: &[&str], counter: &mut u64, out: &mut Vec<Block>) {
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            index += 1;
            continue;
        }

        if let Some(code_info) = trimmed.strip_prefix("```") {
            let info = code_info.trim();
            let mut contents: Vec<&str> = Vec::new();
            let mut end_index = index + 1;
            while end_index < lines.len() {
                if lines[end_index].trim() == "```" {
                    break;
                }
                contents.push(lines[end_index]);
                end_index += 1;
            }
            let text = contents.join("\n");
            let block = Block::new(
                BlockKind::CodeFence {
                    info: if info.is_empty() {
                        None
                    } else {
                        Some(info.to_string())
                    },
                    text,
                },
                next_op_id(counter),
            );
            out.push(block);
            index = (end_index + 1).min(lines.len());
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
            let mut table =
                Table::new(block_id_from_op(elem_id), elem_id, columns, header, elem_id);
            let mut after = None;
            let mut end_index = index + 2;
            while end_index < lines.len() {
                let Some(cells) = parse_table_cells(lines[end_index]) else {
                    break;
                };
                let row_id = next_op_id(counter);
                table.insert_row(after, cells, row_id);
                after = Some(row_id);
                end_index += 1;
            }
            out.push(Block::new(BlockKind::Table { table }, elem_id));
            index = end_index;
            continue;
        }

        // ATX heading: # .. ######
        if let Some((level, title)) = parse_atx_heading(trimmed) {
            let elem_id = next_op_id(counter);
            let text = units_from_str(title, counter, 0);
            out.push(Block::new(BlockKind::Heading { level, text }, elem_id));
            index += 1;
            continue;
        }

        // Unordered / ordered list
        if is_list_start(trimmed) {
            let (list_block, next) = parse_list(lines, index, counter, indent_of(line));
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
            let text = units_from_str(title, counter, 0);
            out.push(Block::new(BlockKind::Heading { level, text }, elem_id));
            index += 2;
            continue;
        }

        let mut paragraph_lines: Vec<&str> = Vec::new();
        let mut end_index = index;
        while end_index < lines.len() {
            let current = lines[end_index];
            let current_trimmed = current.trim();
            if current_trimmed.is_empty()
                || current_trimmed.starts_with("```")
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
        let text = units_from_str(&paragraph_lines.join("\n"), counter, 0);
        out.push(Block::new(BlockKind::Paragraph { text }, elem_id));
        index = end_index;
    }
}

fn parse_table_cells(line: &str) -> Option<Vec<CellContent>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let cells: Vec<_> = inner
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect();
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

fn unordered_marker(trimmed: &str) -> Option<&str> {
    for pref in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(pref) {
            return Some(rest);
        }
    }
    if matches!(trimmed, "-" | "*" | "+") {
        return Some("");
    }
    None
}

fn ordered_marker(trimmed: &str) -> Option<(u32, &str)> {
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
    if let Some(r) = rest.strip_prefix(". ") {
        return Some((num, r));
    }
    if rest == "." {
        return Some((num, ""));
    }
    None
}

/// Parse a list starting at `index` with items at `base_indent` or greater content indent.
fn parse_list(
    lines: &[&str],
    index: usize,
    counter: &mut u64,
    base_indent: usize,
) -> (Block, usize) {
    let first_trim = lines[index].trim();
    let ordered = ordered_marker(first_trim).is_some();
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
            if next_ind >= base_indent && (is_list_start(next_trim) || next_ind > base_indent) {
                i += 1;
                continue;
            }
            break;
        }

        let marker_body = if ordered {
            ordered_marker(trimmed).map(|(_, b)| b)
        } else {
            unordered_marker(trimmed)
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
        let mut children = Vec::new();
        // First paragraph of the item
        let mut para_lines = vec![body];
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
                    i += 1;
                    continue;
                }
                break;
            }
            if cind > base_indent && is_list_start(ctrim) {
                // Nested list
                if !para_lines.is_empty() {
                    let joined = para_lines.join("\n");
                    let eid = next_op_id(counter);
                    let text = units_from_str(&joined, counter, 0);
                    children.push(Block::new(BlockKind::Paragraph { text }, eid));
                    para_lines.clear();
                }
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

        if !para_lines.is_empty() {
            let joined = para_lines.join("\n");
            let eid = next_op_id(counter);
            let text = units_from_str(&joined, counter, 0);
            children.push(Block::new(BlockKind::Paragraph { text }, eid));
        }

        let child_seq =
            Sequence::from_ordered(children.into_iter().map(|b| (b.elem_id, b)).collect());
        items.push(ListItem {
            id: block_id_from_op(item_elem),
            elem_id: item_elem,
            children: child_seq,
        });
    }

    let list_elem = next_op_id(counter);
    let items_seq = Sequence::from_ordered(items.into_iter().map(|it| (it.elem_id, it)).collect());
    let block = Block::new(
        BlockKind::List {
            ordered,
            items: items_seq,
        },
        list_elem,
    );
    (block, i)
}

fn next_op_id(counter: &mut u64) -> OpId {
    let id = OpId {
        counter: *counter,
        peer: 0,
    };
    *counter += 1;
    id
}
