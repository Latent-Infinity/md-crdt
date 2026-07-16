use super::*;

pub(super) fn serialize_block(block: &Block) -> String {
    match &block.kind {
        BlockKind::Paragraph { text } => super::inline::serialize_text(block, text),
        BlockKind::Heading { level, text } => {
            let hashes = "#".repeat((*level).clamp(1, 6) as usize);
            format!("{} {}", hashes, super::inline::serialize_text(block, text))
        }
        BlockKind::List { ordered, items } => serialize_list(*ordered, items, 0),
        BlockKind::CodeFence { info, text } => {
            let mut output = String::from("```");
            if let Some(info) = info {
                output.push_str(info);
            }
            output.push('\n');
            output.push_str(text);
            output.push_str("\n```");
            output
        }
        BlockKind::BlockQuote { children } => {
            let mut rendered = Vec::new();
            for child in children.iter_asc() {
                let child_output = serialize_block(child);
                // Skip empty children (e.g., empty nested blockquotes)
                if !child_output.trim().is_empty() {
                    rendered.push(child_output);
                }
            }
            if rendered.is_empty() {
                // Empty blockquote - return empty string
                return String::new();
            }
            let inner = rendered.join("\n\n");
            inner
                .lines()
                .map(|line| {
                    if line.is_empty() {
                        ">".to_string()
                    } else {
                        format!("> {}", line)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        BlockKind::RawBlock { raw } => raw.clone(),
        BlockKind::Table { table } => serialize_table(table),
    }
}

fn serialize_list(ordered: bool, items: &Sequence<ListItem>, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut lines = Vec::new();
    for (n, item) in items.iter_asc().enumerate() {
        let marker = if ordered {
            format!("{}. ", n + 1)
        } else {
            "- ".to_string()
        };
        let children: Vec<_> = item.children.iter_asc().collect();
        if children.is_empty() {
            lines.push(format!("{pad}{marker}"));
            continue;
        }
        for (ci, child) in children.iter().enumerate() {
            match &child.kind {
                BlockKind::Paragraph { text } => {
                    let body = super::inline::serialize_text(child, text);
                    if ci == 0 {
                        let mut body_lines = body.lines();
                        lines.push(format!(
                            "{pad}{marker}{}",
                            body_lines.next().unwrap_or_default()
                        ));
                        for body_line in body_lines {
                            lines.push(format!("{pad}  {body_line}"));
                        }
                    } else {
                        // Loose list continuation paragraph
                        lines.push(String::new());
                        for bline in body.lines() {
                            lines.push(format!("{pad}  {bline}"));
                        }
                    }
                }
                BlockKind::List {
                    ordered: o2,
                    items: nested,
                } => {
                    let nested_s = serialize_list(*o2, nested, indent + 2);
                    lines.push(nested_s);
                }
                other => {
                    let s = serialize_block(child);
                    if ci == 0 {
                        // first child non-paragraph: put after marker
                        let first = s.lines().next().unwrap_or("");
                        lines.push(format!("{pad}{marker}{first}"));
                        for bline in s.lines().skip(1) {
                            lines.push(format!("{pad}  {bline}"));
                        }
                    } else {
                        lines.push(String::new());
                        for bline in s.lines() {
                            lines.push(format!("{pad}  {bline}"));
                        }
                    }
                    let _ = other;
                }
            }
        }
    }
    lines.join("\n")
}

fn serialize_table(table: &Table) -> String {
    let header = table.header.get();
    let mut rows: Vec<Vec<CellContent>> = table
        .rows
        .iter()
        .filter(|row| !row.deleted.get())
        .map(|row| row.cells.get())
        .collect();
    let columns = table.columns.get();

    let mut col_count = header.len().max(columns.len());
    for row in &rows {
        col_count = col_count.max(row.len());
    }
    if col_count == 0 {
        return String::new();
    }

    let mut header_cells = header;
    header_cells.resize(col_count, String::new());
    let header_line = format!("| {} |", header_cells.join(" | "));

    let mut align_cells = Vec::with_capacity(col_count);
    for idx in 0..col_count {
        let alignment = columns
            .get(idx)
            .map(|col| &col.alignment)
            .unwrap_or(&ColumnAlignment::Left);
        let align = match alignment {
            ColumnAlignment::Left => "---",
            ColumnAlignment::Center => ":---:",
            ColumnAlignment::Right => "---:",
        };
        align_cells.push(align);
    }
    let align_line = format!("| {} |", align_cells.join(" | "));

    let mut rendered_rows = Vec::with_capacity(rows.len());
    for row in &mut rows {
        row.resize(col_count, String::new());
        rendered_rows.push(format!("| {} |", row.join(" | ")));
    }

    let mut output = Vec::with_capacity(2 + rendered_rows.len());
    output.push(header_line);
    output.push(align_line);
    output.extend(rendered_rows);
    output.join("\n")
}

pub(super) fn grapheme_offset_to_byte(text: &str, grapheme_offset: usize) -> Option<usize> {
    if grapheme_offset == 0 {
        return Some(0);
    }

    let mut count = 0;
    for (byte_index, _) in text.grapheme_indices(true) {
        if count == grapheme_offset {
            return Some(byte_index);
        }
        count += 1;
    }
    if count == grapheme_offset {
        Some(text.len())
    } else {
        None
    }
}

pub(super) fn is_grapheme_boundary(text: &str, byte_offset: usize) -> bool {
    if byte_offset == 0 || byte_offset == text.len() {
        return true;
    }
    text.grapheme_indices(true)
        .any(|(index, _)| index == byte_offset)
}

pub(super) fn normalize_structural(text: &str) -> String {
    let mut lines = Vec::new();
    let mut previous_blank = false;
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            if !previous_blank {
                lines.push(String::new());
                previous_blank = true;
            }
        } else {
            lines.push(trimmed.to_string());
            previous_blank = false;
        }
    }

    while lines.first().map(|line| line.is_empty()).unwrap_or(false) {
        lines.remove(0);
    }
    while lines.last().map(|line| line.is_empty()).unwrap_or(false) {
        lines.pop();
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(counter: u64) -> OpId {
        OpId { counter, peer: 1 }
    }

    #[test]
    fn list_serialization_handles_empty_and_non_paragraph_children() {
        let empty_item = ListItem {
            id: block_id_from_op(id(1)),
            elem_id: id(1),
            children: Sequence::new(),
        };
        let empty_items = Sequence::from_ordered(vec![(id(1), empty_item)]);
        assert_eq!(serialize_list(false, &empty_items, 0), "- ");

        let code = Block::new(
            BlockKind::CodeFence {
                info: Some("rs".into()),
                text: "let x = 1;".into(),
            },
            id(3),
        );
        let raw = Block::new(
            BlockKind::RawBlock {
                raw: ":::note".into(),
            },
            id(4),
        );
        let item = ListItem {
            id: block_id_from_op(id(2)),
            elem_id: id(2),
            children: Sequence::from_ordered(vec![(id(3), code), (id(4), raw)]),
        };
        let items = Sequence::from_ordered(vec![(id(2), item)]);
        let rendered = serialize_list(false, &items, 0);
        assert!(rendered.starts_with("- ```rs\n  let x = 1;\n  ```"));
        assert!(rendered.ends_with("\n\n  :::note"));
    }

    #[test]
    fn list_serialization_preserves_inline_marks_in_paragraph_children() {
        let cases = [
            (
                "- **bold** and *italic*\n- `code` and [link](target.md)",
                "- **bold** and *italic*\n- `code` and [link](target.md)",
            ),
            (
                "1. **first**\n2. *second*\n3. [third](third.md)",
                "1. **first**\n2. *second*\n3. [third](third.md)",
            ),
            (
                "- parent\n  - nested **bold**\n  - nested `code`",
                "- parent\n  - nested **bold**\n  - nested `code`",
            ),
            (
                "- first line with **bold**\n  continued with *italic* and [link](next.md)",
                "- first line with **bold**\n  continued with *italic* and [link](next.md)",
            ),
            (
                "- first paragraph\n\n  loose paragraph with `code`\n\n- second item",
                "- first paragraph\n\n  loose paragraph with `code`\n- second item",
            ),
            (
                "- **bold with *nested italic* text**\n- [**bold link**](target.md)",
                "- **bold with *nested italic* text**\n- [**bold link**](target.md)",
            ),
            (
                "- [x] **done**\n- Unicode *café 🇺🇸*\n- escaped \\*literal\\*",
                "- [x] **done**\n- Unicode *café 🇺🇸*\n- escaped \\*literal\\*",
            ),
        ];

        for (markdown, expected) in cases {
            assert_eq!(
                Parser::parse(markdown).serialize(EquivalenceMode::Structural),
                expected,
                "inline marks changed while serializing {markdown:?}"
            );
        }
    }

    #[test]
    fn empty_table_and_grapheme_boundaries_cover_edge_offsets() {
        let table = Table::new(block_id_from_op(id(1)), id(1), vec![], vec![], id(1));
        assert_eq!(serialize_table(&table), "");

        let text = "a👩‍💻b";
        assert_eq!(grapheme_offset_to_byte(text, 0), Some(0));
        assert_eq!(grapheme_offset_to_byte(text, 3), Some(text.len()));
        assert_eq!(grapheme_offset_to_byte(text, 4), None);
        assert!(is_grapheme_boundary(text, 1));
        assert!(!is_grapheme_boundary(text, 2));
        assert!(is_grapheme_boundary(text, text.len()));
    }
}
