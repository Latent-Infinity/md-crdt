//! Markdown document model, parser, serializer, and editing API.

use md_crdt_core::{MarkInterval, MarkSet, OpId, Sequence, TextAnchor};
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

pub type BlockId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub frontmatter: Option<String>,
    pub blocks: Sequence<Block>,
    raw_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub id: BlockId,
    pub elem_id: OpId,
    pub kind: BlockKind,
    pub marks: MarkSet<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph { text: String },
    CodeFence { info: Option<String>, text: String },
    BlockQuote { children: Sequence<Block> },
    RawBlock { raw: String },
    Table { table: Table },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EquivalenceMode {
    Exact,
    Structural,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerializeConfig {
    pub equivalence: EquivalenceMode,
    pub prefer_raw_source: bool,
}

impl SerializeConfig {
    pub fn exact() -> Self {
        Self {
            equivalence: EquivalenceMode::Exact,
            prefer_raw_source: true,
        }
    }

    pub fn structural() -> Self {
        Self {
            equivalence: EquivalenceMode::Structural,
            prefer_raw_source: false,
        }
    }
}

impl Default for SerializeConfig {
    fn default() -> Self {
        Self::exact()
    }
}

pub type RowId = Uuid;
pub type CellContent = String;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnAlignment {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    pub alignment: ColumnAlignment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub id: BlockId,
    pub elem_id: OpId,
    pub deleted: md_crdt_core::LwwRegister<bool>,
    pub columns: md_crdt_core::LwwRegister<Vec<ColumnDef>>,
    pub header: md_crdt_core::LwwRegister<Vec<CellContent>>,
    pub rows: Sequence<TableRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRow {
    pub id: RowId,
    pub elem_id: OpId,
    pub deleted: md_crdt_core::LwwRegister<bool>,
    pub cells: md_crdt_core::LwwRegister<Vec<CellContent>>,
}

impl Table {
    pub fn new(
        id: BlockId,
        elem_id: OpId,
        columns: Vec<ColumnDef>,
        header: Vec<CellContent>,
        op_id: OpId,
    ) -> Self {
        Self {
            id,
            elem_id,
            deleted: md_crdt_core::LwwRegister::new(false, op_id),
            columns: md_crdt_core::LwwRegister::new(columns, op_id),
            header: md_crdt_core::LwwRegister::new(header, op_id),
            rows: Sequence::new(),
        }
    }

    pub fn insert_row(&mut self, after: Option<OpId>, cells: Vec<CellContent>, op_id: OpId) {
        let row = TableRow {
            id: Uuid::new_v4(),
            elem_id: op_id,
            deleted: md_crdt_core::LwwRegister::new(false, op_id),
            cells: md_crdt_core::LwwRegister::new(cells, op_id),
        };
        self.rows.insert(after, row, op_id);
    }

    pub fn remove_row(&mut self, target: OpId, op_id: OpId) {
        // Clone only the necessary row, not twice
        if let Some(existing) = self.rows.get_element(&target)
            && let Some(row) = existing.value.as_ref()
        {
            let mut updated = row.clone();
            updated.deleted.set(true, op_id);
            self.rows.update_value(target, updated);
        }
        self.rows.delete(target, op_id);
    }

    pub fn set_row_cells(&mut self, row_elem_id: OpId, cells: Vec<CellContent>, op_id: OpId) {
        // Clone only once instead of twice
        if let Some(existing) = self.rows.get_element(&row_elem_id)
            && let Some(row) = existing.value.as_ref()
        {
            let mut updated = row.clone();
            updated.cells.set(cells, op_id);
            self.rows.update_value(row_elem_id, updated);
        }
    }

    pub fn set_columns(&mut self, columns: Vec<ColumnDef>, op_id: OpId) {
        self.columns.set(columns, op_id);
    }

    pub fn set_header(&mut self, cells: Vec<CellContent>, op_id: OpId) {
        self.header.set(cells, op_id);
    }

    pub fn rows_in_order(&self) -> Vec<TableRow> {
        self.rows.iter().cloned().collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertTextRun {
    pub block_id: BlockId,
    pub grapheme_offset: usize,
    pub byte_offset: usize,
    pub text: String,
    pub op_id: OpId,
}

pub mod mark_ops {
    use md_crdt_core::mark::{
        Anchor, AnchorBias, MarkInterval, MarkIntervalId, MarkKind, MarkSet, MarkValue,
    };
    use md_crdt_core::{OpId, StateVector};
    use std::collections::BTreeMap;

    pub fn expand_marks_for_insert(
        mark_set: &MarkSet,
        element_order: &[OpId],
        visible_len: usize,
        anchor: Anchor,
        expand: bool,
    ) -> Vec<MarkIntervalId> {
        if !expand {
            return Vec::new();
        }
        let spans = mark_set.render_spans(element_order, visible_len);
        let index = resolve_anchor(&anchor, element_order, visible_len);
        for span in spans {
            if index >= span.start && index < span.end {
                return span.marks;
            }
        }
        Vec::new()
    }

    pub fn lower_remove_mark_range(
        mark_set: &MarkSet,
        interval_id: MarkIntervalId,
        remove_start: Anchor,
        remove_end: Anchor,
        next_id: OpId,
    ) -> (Vec<MarkInterval>, Vec<MarkIntervalId>) {
        let mut new_intervals = Vec::new();
        let mut removed = Vec::new();
        if !mark_set.is_active(&interval_id) {
            return (new_intervals, removed);
        }
        removed.push(interval_id);

        let base = mark_set
            .active_intervals()
            .into_iter()
            .find(|i| i.id == interval_id);
        let Some(interval) = base else {
            return (new_intervals, removed);
        };

        let left_needed = anchor_lt(interval.start, remove_start);
        let right_needed = anchor_lt(remove_end, interval.end);

        if left_needed {
            let mut attrs = BTreeMap::new();
            for (k, v) in &interval.attrs {
                attrs.insert(k.clone(), v.get());
            }
            new_intervals.push(MarkInterval {
                id: next_id,
                kind: interval.kind.clone(),
                start: interval.start,
                end: remove_start,
                attrs: attrs
                    .into_iter()
                    .map(|(k, v)| (k, md_crdt_core::LwwRegister::new(v, next_id)))
                    .collect(),
                op_id: next_id,
            });
        }

        if right_needed {
            let mut attrs = BTreeMap::new();
            for (k, v) in &interval.attrs {
                attrs.insert(k.clone(), v.get());
            }
            let id = OpId {
                counter: next_id.counter + 1,
                peer: next_id.peer,
            };
            new_intervals.push(MarkInterval {
                id,
                kind: interval.kind.clone(),
                start: remove_end,
                end: interval.end,
                attrs: attrs
                    .into_iter()
                    .map(|(k, v)| (k, md_crdt_core::LwwRegister::new(v, id)))
                    .collect(),
                op_id: id,
            });
        }

        (new_intervals, removed)
    }

    pub fn remove_mark_observed(
        interval_id: MarkIntervalId,
        observed: StateVector,
        op_id: OpId,
    ) -> (MarkIntervalId, StateVector, OpId) {
        (interval_id, observed, op_id)
    }

    fn resolve_anchor(anchor: &Anchor, order: &[OpId], len: usize) -> usize {
        let idx = order
            .iter()
            .position(|id| *id == anchor.elem_id)
            .unwrap_or(0);
        match anchor.bias {
            AnchorBias::Before => idx,
            AnchorBias::After => (idx + 1).min(len),
        }
    }

    fn anchor_lt(a: Anchor, b: Anchor) -> bool {
        (a.elem_id, bias_rank(a.bias)) < (b.elem_id, bias_rank(b.bias))
    }

    fn bias_rank(bias: AnchorBias) -> u8 {
        match bias {
            AnchorBias::Before => 0,
            AnchorBias::After => 1,
        }
    }

    pub fn sample_interval(id: OpId, start: OpId, end: OpId) -> MarkInterval {
        MarkInterval {
            id,
            kind: MarkKind::Bold,
            start: Anchor {
                elem_id: start,
                bias: AnchorBias::Before,
            },
            end: Anchor {
                elem_id: end,
                bias: AnchorBias::After,
            },
            attrs: BTreeMap::new(),
            op_id: id,
        }
    }

    pub fn mark_value_string(value: &str) -> MarkValue {
        MarkValue::String(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOp {
    InsertText(InsertTextRun),
    AddMark {
        interval: MarkInterval<String, String>,
    },
    RemoveMark {
        add_id: OpId,
        remove_id: OpId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("block not found")]
    BlockNotFound,
    #[error("invalid offset")]
    InvalidOffset,
    #[error("invalid grapheme boundary")]
    InvalidGraphemeBoundary,
}

impl Document {
    pub fn new() -> Self {
        Self {
            frontmatter: None,
            blocks: Sequence::new(),
            raw_source: None,
        }
    }

    pub fn set_raw_source(&mut self, source: String) {
        self.raw_source = Some(source);
    }

    pub fn clear_raw_source(&mut self) {
        self.raw_source = None;
    }

    pub fn blocks_in_order(&self) -> Vec<&Block> {
        self.blocks.iter_asc().collect()
    }

    pub fn insert_text(
        &mut self,
        block_id: BlockId,
        grapheme_offset: usize,
        text: &str,
        op_id: OpId,
    ) -> Result<Vec<EditOp>, EditError> {
        // Find block's elem_id by block_id (O(n) search, unavoidable without block_id index)
        let elem_id = self
            .blocks
            .iter_asc()
            .find(|block| block.id == block_id)
            .map(|block| block.elem_id)
            .ok_or(EditError::BlockNotFound)?;

        // Get element via O(1) lookup and clone block for modification
        let Some(existing) = self.blocks.get_element(&elem_id) else {
            return Err(EditError::BlockNotFound);
        };
        let Some(block) = existing.value.as_ref() else {
            return Err(EditError::BlockNotFound);
        };

        let mut updated = block.clone();
        let BlockKind::Paragraph { text: ref mut body } = updated.kind else {
            return Err(EditError::InvalidOffset);
        };

        let byte_offset =
            grapheme_offset_to_byte(body, grapheme_offset).ok_or(EditError::InvalidOffset)?;
        body.insert_str(byte_offset, text);

        // O(1) update instead of O(n) sequence rebuild
        self.blocks.update_value(elem_id, updated);
        self.clear_raw_source();

        Ok(vec![EditOp::InsertText(InsertTextRun {
            block_id,
            grapheme_offset,
            byte_offset,
            text: text.to_string(),
            op_id,
        })])
    }

    pub fn raw_apply_op(
        &mut self,
        op: EditOp,
        validate_grapheme_boundaries: bool,
    ) -> Result<(), EditError> {
        match op {
            EditOp::InsertText(run) => {
                // Find block's elem_id by block_id
                let elem_id = self
                    .blocks
                    .iter_asc()
                    .find(|block| block.id == run.block_id)
                    .map(|block| block.elem_id)
                    .ok_or(EditError::BlockNotFound)?;

                // Get element via O(1) lookup and clone block for modification
                let Some(existing) = self.blocks.get_element(&elem_id) else {
                    return Err(EditError::BlockNotFound);
                };
                let Some(block) = existing.value.as_ref() else {
                    return Err(EditError::BlockNotFound);
                };

                let mut updated = block.clone();
                let BlockKind::Paragraph { text: ref mut body } = updated.kind else {
                    return Err(EditError::InvalidOffset);
                };

                let byte_offset = run.byte_offset;
                if byte_offset > body.len() {
                    return Err(EditError::InvalidOffset);
                }
                if !body.is_char_boundary(byte_offset) {
                    return Err(EditError::InvalidOffset);
                }
                if validate_grapheme_boundaries && !is_grapheme_boundary(body, byte_offset) {
                    return Err(EditError::InvalidGraphemeBoundary);
                }
                body.insert_str(byte_offset, &run.text);

                // O(1) update instead of O(n) sequence rebuild
                self.blocks.update_value(elem_id, updated);
                self.clear_raw_source();
                Ok(())
            }
            EditOp::AddMark { interval } => {
                // Only clone and update the target block with matching elem_id
                let target_elem_id = interval.id;
                if let Some(existing) = self.blocks.get_element(&target_elem_id)
                    && let Some(block) = existing.value.as_ref()
                {
                    let mut updated = block.clone();
                    updated.marks.add(interval);
                    self.blocks.update_value(target_elem_id, updated);
                }
                self.clear_raw_source();
                Ok(())
            }
            EditOp::RemoveMark { add_id, remove_id } => {
                // Only clone and update blocks that actually have the mark
                // Collect elem_ids first to avoid borrow conflict
                let elem_ids_with_mark: Vec<_> = self
                    .blocks
                    .iter_asc()
                    .filter(|block| block.marks.interval(&add_id).is_some())
                    .map(|block| block.elem_id)
                    .collect();

                for elem_id in elem_ids_with_mark {
                    if let Some(existing) = self.blocks.get_element(&elem_id)
                        && let Some(block) = existing.value.as_ref()
                    {
                        let mut updated = block.clone();
                        updated.marks.remove(add_id, remove_id);
                        self.blocks.update_value(elem_id, updated);
                    }
                }
                self.clear_raw_source();
                Ok(())
            }
        }
    }

    pub fn remove_mark(
        &mut self,
        block_id: BlockId,
        add_id: OpId,
        remove_id: OpId,
        remove_start: TextAnchor,
        remove_end: TextAnchor,
    ) -> Result<Vec<EditOp>, EditError> {
        // Find block's elem_id by block_id
        let elem_id = self
            .blocks
            .iter_asc()
            .find(|block| block.id == block_id)
            .map(|block| block.elem_id)
            .ok_or(EditError::BlockNotFound)?;

        // Get element via O(1) lookup and clone block for modification
        let Some(existing) = self.blocks.get_element(&elem_id) else {
            return Err(EditError::BlockNotFound);
        };
        let Some(block) = existing.value.as_ref() else {
            return Err(EditError::BlockNotFound);
        };

        let Some(interval) = block.marks.interval(&add_id).cloned() else {
            return Err(EditError::InvalidOffset);
        };

        let mut updated = block.clone();
        let mut ops = Vec::new();
        updated.marks.remove(add_id, remove_id);
        ops.push(EditOp::RemoveMark { add_id, remove_id });

        let left_needed = remove_start > interval.start;
        let right_needed = remove_end < interval.end;

        if left_needed {
            let left_id = OpId {
                counter: add_id.counter + 1,
                peer: add_id.peer,
            };
            let mut left = MarkInterval::new(left_id, interval.start, remove_start);
            left.attributes = interval.attributes.clone();
            updated.marks.add(left.clone());
            ops.push(EditOp::AddMark { interval: left });
        }

        if right_needed {
            let right_id = OpId {
                counter: add_id.counter + 2,
                peer: add_id.peer,
            };
            let mut right = MarkInterval::new(right_id, remove_end, interval.end);
            right.attributes = interval.attributes.clone();
            updated.marks.add(right.clone());
            ops.push(EditOp::AddMark { interval: right });
        }

        // O(1) update instead of O(n) sequence rebuild
        self.blocks.update_value(elem_id, updated);
        self.clear_raw_source();

        Ok(ops)
    }

    pub fn serialize(&self, mode: EquivalenceMode) -> String {
        let config = SerializeConfig {
            equivalence: mode,
            prefer_raw_source: true,
        };
        self.serialize_with_config(&config)
    }

    pub fn serialize_with_config(&self, config: &SerializeConfig) -> String {
        if let EquivalenceMode::Exact = config.equivalence
            && config.prefer_raw_source
            && let Some(raw) = &self.raw_source
        {
            return raw.clone();
        }

        let mut output = String::new();
        if let Some(frontmatter) = &self.frontmatter {
            output.push_str("---\n");
            output.push_str(frontmatter);
            output.push_str("\n---\n\n");
        }

        let blocks = self.blocks_in_order();
        for (index, block) in blocks.iter().enumerate() {
            if index > 0 {
                output.push_str("\n\n");
            }
            output.push_str(&serialize_block(block));
        }

        match config.equivalence {
            EquivalenceMode::Exact => output,
            EquivalenceMode::Structural => normalize_structural(&output),
        }
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Block {
    pub fn new(kind: BlockKind, insert_id: OpId) -> Self {
        Self {
            id: Uuid::new_v4(),
            elem_id: insert_id,
            kind,
            marks: MarkSet::new(),
        }
    }
}

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
            blocks: sequence,
            raw_source: Some(text.to_string()),
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

        let mut paragraph_lines: Vec<&str> = Vec::new();
        let mut end_index = index;
        while end_index < lines.len() {
            let current = lines[end_index];
            let current_trimmed = current.trim();
            if current_trimmed.is_empty()
                || current_trimmed.starts_with("```")
                || current_trimmed.starts_with('>')
                || current_trimmed.starts_with(":::")
            {
                break;
            }
            paragraph_lines.push(current);
            end_index += 1;
        }
        let block = Block::new(
            BlockKind::Paragraph {
                text: paragraph_lines.join("\n"),
            },
            next_op_id(counter),
        );
        out.push(block);
        index = end_index;
    }
}

fn next_op_id(counter: &mut u64) -> OpId {
    let id = OpId {
        counter: *counter,
        peer: 0,
    };
    *counter += 1;
    id
}

fn serialize_block(block: &Block) -> String {
    match &block.kind {
        BlockKind::Paragraph { text } => text.clone(),
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

fn grapheme_offset_to_byte(text: &str, grapheme_offset: usize) -> Option<usize> {
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

fn is_grapheme_boundary(text: &str, byte_offset: usize) -> bool {
    if byte_offset == 0 || byte_offset == text.len() {
        return true;
    }
    text.grapheme_indices(true)
        .any(|(index, _)| index == byte_offset)
}

fn normalize_structural(text: &str) -> String {
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
    use crate::mark_ops;
    use md_crdt_core::mark::{Anchor, AnchorBias, MarkKind, MarkSet, MarkValue};
    use std::collections::BTreeMap;

    #[test]
    fn test_block_ids_and_elem_ids() {
        let id = OpId {
            counter: 1,
            peer: 1,
        };
        let block_a = Block::new(BlockKind::Paragraph { text: "A".into() }, id);
        let block_b = Block::new(BlockKind::Paragraph { text: "B".into() }, id);
        assert_ne!(block_a.id, block_b.id);
        assert_eq!(block_a.elem_id, id);
    }

    #[test]
    fn test_blockquote_hierarchy() {
        let child = Block::new(
            BlockKind::Paragraph {
                text: "Nested".into(),
            },
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let mut children = Sequence::new();
        children.apply_op((child.elem_id, child.clone()));
        let quote = Block::new(
            BlockKind::BlockQuote { children },
            OpId {
                counter: 2,
                peer: 0,
            },
        );
        if let BlockKind::BlockQuote { children } = &quote.kind {
            let collected: Vec<_> = children.iter().collect();
            assert_eq!(collected.len(), 1);
            assert_eq!(collected[0].kind, child.kind);
        } else {
            panic!("Expected blockquote block");
        }
    }

    #[test]
    fn test_exact_equivalence_roundtrip() {
        let input = "---\ntitle: Test\n---\n\nHello\n\n```rust\nfn main() {}\n```\n\n:::custom\nraw block\n";
        let doc = Parser::parse(input);
        let output = doc.serialize(EquivalenceMode::Exact);
        assert_eq!(output, input);
    }

    #[test]
    fn test_structural_equivalence() {
        let a = "Hello\n\nWorld\n";
        let b = "Hello\n\n\nWorld";
        let doc_a = Parser::parse(a);
        let doc_b = Parser::parse(b);
        let norm_a = doc_a.serialize(EquivalenceMode::Structural);
        let norm_b = doc_b.serialize(EquivalenceMode::Structural);
        assert_eq!(norm_a, norm_b);
    }

    #[test]
    fn test_raw_block_preservation() {
        let input = ":::custom\nraw line\n\nNext";
        let doc = Parser::parse(input);
        let output = doc.serialize(EquivalenceMode::Structural);
        assert!(output.contains(":::custom\nraw line"));
    }

    #[test]
    fn test_insert_text_grapheme_offsets() {
        let mut doc = Document::new();
        let block = Block::new(
            BlockKind::Paragraph {
                text: "a🇺🇸b".into(),
            },
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let block_id = block.id;
        doc.blocks.apply_op((block.elem_id, block));

        let ops = doc
            .insert_text(
                block_id,
                1,
                "X",
                OpId {
                    counter: 2,
                    peer: 0,
                },
            )
            .unwrap();
        match &ops[0] {
            EditOp::InsertText(run) => {
                assert_eq!(run.grapheme_offset, 1);
                assert_eq!(run.text, "X");
                assert!(run.byte_offset > 0);
            }
            _ => panic!("Expected insert op"),
        }

        let updated = doc.blocks.iter().next().unwrap();
        if let BlockKind::Paragraph { text } = &updated.kind {
            assert_eq!(text, "aX🇺🇸b");
        } else {
            panic!("Expected paragraph block");
        }
    }

    #[test]
    fn test_remove_mark_split() {
        let mut doc = Document::new();
        let mut block = Block::new(
            BlockKind::Paragraph {
                text: "Hello".into(),
            },
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let interval = MarkInterval::new(
            OpId {
                counter: 10,
                peer: 0,
            },
            TextAnchor {
                op_id: OpId {
                    counter: 1,
                    peer: 0,
                },
            },
            TextAnchor {
                op_id: OpId {
                    counter: 5,
                    peer: 0,
                },
            },
        );
        block.marks.add(interval.clone());
        let block_id = block.id;
        doc.blocks.apply_op((block.elem_id, block));

        let ops = doc
            .remove_mark(
                block_id,
                interval.id,
                OpId {
                    counter: 20,
                    peer: 0,
                },
                TextAnchor {
                    op_id: OpId {
                        counter: 2,
                        peer: 0,
                    },
                },
                TextAnchor {
                    op_id: OpId {
                        counter: 4,
                        peer: 0,
                    },
                },
            )
            .unwrap();

        let add_ops = ops
            .iter()
            .filter(|op| matches!(op, EditOp::AddMark { .. }))
            .count();
        assert_eq!(add_ops, 2, "Expected split into two add ops");
    }

    #[test]
    fn test_raw_apply_op_grapheme_validation() {
        let mut doc = Document::new();
        let block = Block::new(
            BlockKind::Paragraph {
                text: "a🇺🇸b".into(),
            },
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let block_id = block.id;
        doc.blocks.apply_op((block.elem_id, block));

        let mut chars = "a🇺🇸b".char_indices();
        chars.next();
        let _first_flag = chars.next().unwrap().0;
        let bad_offset = chars.next().unwrap().0;
        let op = EditOp::InsertText(InsertTextRun {
            block_id,
            grapheme_offset: 0,
            byte_offset: bad_offset,
            text: "X".into(),
            op_id: OpId {
                counter: 2,
                peer: 0,
            },
        });

        assert_eq!(
            doc.raw_apply_op(op.clone(), true),
            Err(EditError::InvalidGraphemeBoundary)
        );
        assert!(doc.raw_apply_op(op, false).is_ok());
    }

    #[test]
    fn test_insert_text_run_mark_expansion() {
        let mut set = MarkSet::new();
        let id = OpId {
            counter: 1,
            peer: 1,
        };
        let start = Anchor {
            elem_id: OpId {
                counter: 1,
                peer: 1,
            },
            bias: AnchorBias::Before,
        };
        let end = Anchor {
            elem_id: OpId {
                counter: 2,
                peer: 1,
            },
            bias: AnchorBias::After,
        };
        let mut attrs = BTreeMap::new();
        attrs.insert("k".to_string(), MarkValue::String("v".into()));
        set.set_mark(id, MarkKind::Bold, start, end, attrs, id);

        let order = vec![
            OpId {
                counter: 1,
                peer: 1,
            },
            OpId {
                counter: 2,
                peer: 1,
            },
        ];
        let marks = mark_ops::expand_marks_for_insert(
            &set,
            &order,
            2,
            Anchor {
                elem_id: OpId {
                    counter: 1,
                    peer: 1,
                },
                bias: AnchorBias::After,
            },
            true,
        );
        assert_eq!(marks, vec![id]);
    }

    #[test]
    fn test_insert_text_run_no_expand() {
        let set = MarkSet::new();
        let order = vec![OpId {
            counter: 1,
            peer: 1,
        }];
        let marks = mark_ops::expand_marks_for_insert(
            &set,
            &order,
            1,
            Anchor {
                elem_id: OpId {
                    counter: 1,
                    peer: 1,
                },
                bias: AnchorBias::Before,
            },
            false,
        );
        assert!(marks.is_empty());
    }

    #[test]
    fn test_remove_mark_range_splits_interval() {
        let mut set = MarkSet::new();
        let id = OpId {
            counter: 1,
            peer: 1,
        };
        let start = Anchor {
            elem_id: OpId {
                counter: 1,
                peer: 1,
            },
            bias: AnchorBias::Before,
        };
        let end = Anchor {
            elem_id: OpId {
                counter: 3,
                peer: 1,
            },
            bias: AnchorBias::After,
        };
        set.set_mark(id, MarkKind::Bold, start, end, BTreeMap::new(), id);

        let (new_intervals, removed) = mark_ops::lower_remove_mark_range(
            &set,
            id,
            Anchor {
                elem_id: OpId {
                    counter: 2,
                    peer: 1,
                },
                bias: AnchorBias::Before,
            },
            Anchor {
                elem_id: OpId {
                    counter: 2,
                    peer: 1,
                },
                bias: AnchorBias::After,
            },
            OpId {
                counter: 10,
                peer: 1,
            },
        );
        assert_eq!(removed, vec![id]);
        assert_eq!(new_intervals.len(), 2);
    }

    #[test]
    fn test_remove_mark_range_full() {
        let mut set = MarkSet::new();
        let id = OpId {
            counter: 1,
            peer: 1,
        };
        let start = Anchor {
            elem_id: OpId {
                counter: 1,
                peer: 1,
            },
            bias: AnchorBias::Before,
        };
        let end = Anchor {
            elem_id: OpId {
                counter: 2,
                peer: 1,
            },
            bias: AnchorBias::After,
        };
        set.set_mark(id, MarkKind::Bold, start, end, BTreeMap::new(), id);

        let (new_intervals, removed) = mark_ops::lower_remove_mark_range(
            &set,
            id,
            start,
            end,
            OpId {
                counter: 10,
                peer: 1,
            },
        );
        assert_eq!(removed, vec![id]);
        assert!(new_intervals.is_empty());
    }

    #[test]
    fn test_insert_text_error_cases() {
        let mut doc = Document::new();
        let block = Block::new(
            BlockKind::Paragraph {
                text: "Hello".into(),
            },
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let block_id = block.id;
        doc.blocks.apply_op((block.elem_id, block.clone()));

        // Test BlockNotFound
        let fake_id = Uuid::new_v4();
        let result = doc.insert_text(
            fake_id,
            0,
            "X",
            OpId {
                counter: 2,
                peer: 0,
            },
        );
        assert_eq!(result, Err(EditError::BlockNotFound));

        // Test InvalidOffset (too large)
        let result = doc.insert_text(
            block_id,
            1000,
            "X",
            OpId {
                counter: 2,
                peer: 0,
            },
        );
        assert_eq!(result, Err(EditError::InvalidOffset));

        // Test inserting into non-paragraph block
        let code_block = Block::new(
            BlockKind::CodeFence {
                info: None,
                text: "code".into(),
            },
            OpId {
                counter: 3,
                peer: 0,
            },
        );
        let code_id = code_block.id;
        doc.blocks.apply_op((code_block.elem_id, code_block));

        let result = doc.insert_text(
            code_id,
            0,
            "X",
            OpId {
                counter: 4,
                peer: 0,
            },
        );
        assert_eq!(result, Err(EditError::InvalidOffset));
    }

    #[test]
    fn test_raw_apply_op_error_cases() {
        let mut doc = Document::new();
        let block = Block::new(
            BlockKind::Paragraph {
                text: "Hello".into(),
            },
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let block_id = block.id;
        doc.blocks.apply_op((block.elem_id, block));

        // Test BlockNotFound for InsertText
        let fake_id = Uuid::new_v4();
        let op = EditOp::InsertText(InsertTextRun {
            block_id: fake_id,
            grapheme_offset: 0,
            byte_offset: 0,
            text: "X".into(),
            op_id: OpId {
                counter: 2,
                peer: 0,
            },
        });
        assert_eq!(doc.raw_apply_op(op, false), Err(EditError::BlockNotFound));

        // Test InvalidOffset (byte offset too large)
        let op = EditOp::InsertText(InsertTextRun {
            block_id,
            grapheme_offset: 0,
            byte_offset: 1000,
            text: "X".into(),
            op_id: OpId {
                counter: 2,
                peer: 0,
            },
        });
        assert_eq!(doc.raw_apply_op(op, false), Err(EditError::InvalidOffset));

        // Test InvalidOffset (not on char boundary)
        // Create block with multi-byte character to properly test char boundaries
        let mut doc2 = Document::new();
        let mb_block = Block::new(
            BlockKind::Paragraph {
                text: "こんにちは".into(), // Japanese text
            },
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let mb_id = mb_block.id;
        doc2.blocks.apply_op((mb_block.elem_id, mb_block));

        // Try to insert at invalid byte offset (middle of character)
        let op = EditOp::InsertText(InsertTextRun {
            block_id: mb_id,
            grapheme_offset: 0,
            byte_offset: 1, // Invalid: middle of multi-byte character
            text: "X".into(),
            op_id: OpId {
                counter: 2,
                peer: 0,
            },
        });
        assert_eq!(doc2.raw_apply_op(op, false), Err(EditError::InvalidOffset));

        // Test inserting into non-paragraph block
        let code_block = Block::new(
            BlockKind::CodeFence {
                info: None,
                text: "code".into(),
            },
            OpId {
                counter: 3,
                peer: 0,
            },
        );
        let code_id = code_block.id;
        doc.blocks.apply_op((code_block.elem_id, code_block));

        let op = EditOp::InsertText(InsertTextRun {
            block_id: code_id,
            grapheme_offset: 0,
            byte_offset: 0,
            text: "X".into(),
            op_id: OpId {
                counter: 4,
                peer: 0,
            },
        });
        assert_eq!(doc.raw_apply_op(op, false), Err(EditError::InvalidOffset));
    }

    #[test]
    fn test_raw_apply_op_add_mark() {
        let mut doc = Document::new();
        let block = Block::new(
            BlockKind::Paragraph {
                text: "Hello".into(),
            },
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let block_elem_id = block.elem_id;
        doc.blocks.apply_op((block.elem_id, block));

        // Create and apply AddMark operation
        let interval = MarkInterval::new(
            block_elem_id,
            TextAnchor {
                op_id: OpId {
                    counter: 0,
                    peer: 0,
                },
            },
            TextAnchor {
                op_id: OpId {
                    counter: 5,
                    peer: 0,
                },
            },
        );

        let op = EditOp::AddMark {
            interval: interval.clone(),
        };
        assert!(doc.raw_apply_op(op, false).is_ok());

        // Verify the mark was added
        let updated_block = doc.blocks.iter().next().unwrap();
        assert!(updated_block.marks.is_active(&block_elem_id));
    }

    #[test]
    fn test_remove_mark_error_cases() {
        let mut doc = Document::new();
        let block = Block::new(
            BlockKind::Paragraph {
                text: "Hello".into(),
            },
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let block_id = block.id;
        doc.blocks.apply_op((block.elem_id, block));

        // Test BlockNotFound
        let fake_id = Uuid::new_v4();
        let result = doc.remove_mark(
            fake_id,
            OpId {
                counter: 10,
                peer: 0,
            },
            OpId {
                counter: 20,
                peer: 0,
            },
            TextAnchor {
                op_id: OpId {
                    counter: 2,
                    peer: 0,
                },
            },
            TextAnchor {
                op_id: OpId {
                    counter: 4,
                    peer: 0,
                },
            },
        );
        assert_eq!(result, Err(EditError::BlockNotFound));

        // Test mark not found (InvalidOffset)
        let result = doc.remove_mark(
            block_id,
            OpId {
                counter: 999,
                peer: 0,
            },
            OpId {
                counter: 20,
                peer: 0,
            },
            TextAnchor {
                op_id: OpId {
                    counter: 2,
                    peer: 0,
                },
            },
            TextAnchor {
                op_id: OpId {
                    counter: 4,
                    peer: 0,
                },
            },
        );
        assert_eq!(result, Err(EditError::InvalidOffset));
    }

    #[test]
    fn test_set_raw_source() {
        let mut doc = Document::new();
        assert_eq!(doc.raw_source, None);

        doc.set_raw_source("test".into());
        assert_eq!(doc.raw_source, Some("test".into()));

        doc.clear_raw_source();
        assert_eq!(doc.raw_source, None);
    }

    #[test]
    fn test_parser_edge_cases() {
        // Test empty document
        let doc = Parser::parse("");
        assert!(doc.blocks.iter().next().is_none());

        // Test only whitespace
        let doc = Parser::parse("\n\n\n");
        assert!(doc.blocks.iter().next().is_none());

        // Test unclosed code fence
        let doc = Parser::parse("```rust\ncode\n");
        let blocks: Vec<_> = doc.blocks.iter().collect();
        assert_eq!(blocks.len(), 1);
        if let BlockKind::CodeFence { info, text } = &blocks[0].kind {
            assert_eq!(info, &Some("rust".into()));
            assert_eq!(text, "code");
        } else {
            panic!("Expected code fence");
        }

        // Test empty code fence
        let doc = Parser::parse("```\n```");
        let blocks: Vec<_> = doc.blocks.iter().collect();
        assert_eq!(blocks.len(), 1);
        if let BlockKind::CodeFence { info, text } = &blocks[0].kind {
            assert_eq!(info, &None);
            assert_eq!(text, "");
        } else {
            panic!("Expected code fence");
        }

        // Test blockquote at end
        let doc = Parser::parse("> Quote");
        let blocks: Vec<_> = doc.blocks.iter().collect();
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0].kind, BlockKind::BlockQuote { .. }));

        // Test raw block at end
        let doc = Parser::parse(":::raw\nContent");
        let blocks: Vec<_> = doc.blocks.iter().collect();
        assert_eq!(blocks.len(), 1);
        if let BlockKind::RawBlock { raw } = &blocks[0].kind {
            assert!(raw.contains(":::raw"));
        } else {
            panic!("Expected raw block");
        }
    }

    #[test]
    fn test_serialize_structural_edge_cases() {
        // Test normalization removes leading/trailing blank lines
        let doc = Parser::parse("\n\nHello\n\n\n");
        let output = doc.serialize(EquivalenceMode::Structural);
        assert_eq!(output, "Hello");

        // Test normalization collapses multiple blank lines
        let doc = Parser::parse("A\n\n\n\nB");
        let output = doc.serialize(EquivalenceMode::Structural);
        assert_eq!(output, "A\n\nB");

        // Test normalization trims trailing whitespace
        let doc = Parser::parse("Hello   \nWorld  ");
        let output = doc.serialize(EquivalenceMode::Structural);
        assert_eq!(output, "Hello\nWorld");
    }

    #[test]
    fn test_grapheme_offset_edge_cases() {
        // Test empty string
        assert_eq!(grapheme_offset_to_byte("", 0), Some(0));
        assert_eq!(grapheme_offset_to_byte("", 1), None);

        // Test offset at end
        assert_eq!(grapheme_offset_to_byte("abc", 3), Some(3));

        // Test with multi-byte grapheme clusters
        let text = "a🇺🇸b";
        assert_eq!(grapheme_offset_to_byte(text, 0), Some(0));
        assert_eq!(grapheme_offset_to_byte(text, 1), Some(1));
        assert_eq!(grapheme_offset_to_byte(text, 2), Some(9)); // After flag
        assert_eq!(grapheme_offset_to_byte(text, 3), Some(10)); // At end
    }

    #[test]
    fn test_is_grapheme_boundary() {
        // Test boundaries
        assert!(is_grapheme_boundary("abc", 0));
        assert!(is_grapheme_boundary("abc", 3));
        assert!(is_grapheme_boundary("abc", 1));

        // Test with multi-byte
        let text = "a🇺🇸b";
        assert!(is_grapheme_boundary(text, 0));
        assert!(is_grapheme_boundary(text, 1)); // After 'a'
        assert!(!is_grapheme_boundary(text, 2)); // Middle of flag
        assert!(is_grapheme_boundary(text, 9)); // After flag
        assert!(is_grapheme_boundary(text, text.len()));
    }

    #[test]
    fn test_raw_apply_op_remove_mark() {
        let mut doc = Document::new();
        let block = Block::new(
            BlockKind::Paragraph {
                text: "Hello".into(),
            },
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        doc.blocks.apply_op((block.elem_id, block));

        // Apply RemoveMark operation
        let op = EditOp::RemoveMark {
            add_id: OpId {
                counter: 10,
                peer: 0,
            },
            remove_id: OpId {
                counter: 20,
                peer: 0,
            },
        };
        assert!(doc.raw_apply_op(op, false).is_ok());
        assert_eq!(doc.raw_source, None);
    }

    #[test]
    fn test_serialize_with_frontmatter_structural() {
        let input = "---\ntitle: Test\nauthor: Me\n---\n\nContent";
        let doc = Parser::parse(input);
        let output = doc.serialize(EquivalenceMode::Structural);
        assert!(output.contains("---\n"));
        assert!(output.contains("title: Test"));
        assert!(output.contains("author: Me"));
        assert!(output.contains("Content"));
    }

    #[test]
    fn test_serialize_code_fence_with_info() {
        let input = "```rust\nfn main() {}\n```";
        let doc = Parser::parse(input);
        let output = doc.serialize(EquivalenceMode::Structural);
        assert!(output.contains("```rust\n"));
        assert!(output.contains("fn main() {}"));
        assert!(output.contains("\n```"));
    }

    #[test]
    fn test_serialize_blockquote_with_children() {
        let input = "> Line 1\n> Line 2\n> \n> Line 3";
        let doc = Parser::parse(input);
        let output = doc.serialize(EquivalenceMode::Structural);
        assert!(output.contains("> Line 1"));
        assert!(output.contains("> Line 2"));
        assert!(output.contains("> Line 3"));
    }

    #[test]
    fn test_parser_blockquote_multiline() {
        let input = "> First\n> Second\nNot quote";
        let doc = Parser::parse(input);
        let blocks: Vec<_> = doc.blocks.iter().collect();
        // Parser creates blockquote and paragraph
        assert!(!blocks.is_empty());

        // Find the blockquote
        let has_blockquote = blocks
            .iter()
            .any(|b| matches!(b.kind, BlockKind::BlockQuote { .. }));
        assert!(has_blockquote, "Expected at least one blockquote");

        // Verify blockquote has children
        for block in &blocks {
            if let BlockKind::BlockQuote { children } = &block.kind {
                let child_blocks: Vec<_> = children.iter().collect();
                assert!(!child_blocks.is_empty(), "Blockquote should have children");
            }
        }
    }

    #[test]
    fn test_insert_text_updates_all_blocks() {
        let mut doc = Document::new();
        let block1 = Block::new(
            BlockKind::Paragraph {
                text: "First".into(),
            },
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let block2 = Block::new(
            BlockKind::Paragraph {
                text: "Second".into(),
            },
            OpId {
                counter: 2,
                peer: 0,
            },
        );
        let block1_id = block1.id;
        doc.blocks.apply_op((block1.elem_id, block1));
        doc.blocks.apply_op((block2.elem_id, block2));

        // Insert into first block
        let result = doc.insert_text(
            block1_id,
            0,
            "X",
            OpId {
                counter: 3,
                peer: 0,
            },
        );
        assert!(result.is_ok());

        // Verify both blocks are still present
        let blocks: Vec<_> = doc.blocks.iter().collect();
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn test_raw_apply_op_updates_all_blocks() {
        let mut doc = Document::new();
        let block1 = Block::new(
            BlockKind::Paragraph {
                text: "First".into(),
            },
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let block2 = Block::new(
            BlockKind::Paragraph {
                text: "Second".into(),
            },
            OpId {
                counter: 2,
                peer: 0,
            },
        );
        let block1_elem_id = block1.elem_id;
        doc.blocks.apply_op((block1.elem_id, block1));
        doc.blocks.apply_op((block2.elem_id, block2));

        // Add mark to first block via raw_apply_op
        let interval = MarkInterval::new(
            block1_elem_id,
            TextAnchor {
                op_id: OpId {
                    counter: 0,
                    peer: 0,
                },
            },
            TextAnchor {
                op_id: OpId {
                    counter: 5,
                    peer: 0,
                },
            },
        );
        let op = EditOp::AddMark { interval };
        assert!(doc.raw_apply_op(op, false).is_ok());

        // Verify both blocks are still present
        let blocks: Vec<_> = doc.blocks.iter().collect();
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn test_remove_mark_updates_all_blocks() {
        let mut doc = Document::new();
        let block1 = Block::new(
            BlockKind::Paragraph {
                text: "First".into(),
            },
            OpId {
                counter: 1,
                peer: 0,
            },
        );
        let block2 = Block::new(
            BlockKind::Paragraph {
                text: "Second".into(),
            },
            OpId {
                counter: 2,
                peer: 0,
            },
        );

        // Add mark to block1
        let mut block1 = block1;
        let interval = MarkInterval::new(
            OpId {
                counter: 10,
                peer: 0,
            },
            TextAnchor {
                op_id: OpId {
                    counter: 0,
                    peer: 0,
                },
            },
            TextAnchor {
                op_id: OpId {
                    counter: 5,
                    peer: 0,
                },
            },
        );
        block1.marks.add(interval.clone());
        let block1_id = block1.id;
        doc.blocks.apply_op((block1.elem_id, block1));
        doc.blocks.apply_op((block2.elem_id, block2));

        // Remove the mark
        let result = doc.remove_mark(
            block1_id,
            OpId {
                counter: 10,
                peer: 0,
            },
            OpId {
                counter: 20,
                peer: 0,
            },
            TextAnchor {
                op_id: OpId {
                    counter: 1,
                    peer: 0,
                },
            },
            TextAnchor {
                op_id: OpId {
                    counter: 4,
                    peer: 0,
                },
            },
        );
        assert!(result.is_ok());

        // Verify both blocks are still present
        let blocks: Vec<_> = doc.blocks.iter().collect();
        assert_eq!(blocks.len(), 2);
    }
}
