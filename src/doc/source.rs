use super::{Block, BlockId, BlockKind};
use crate::core::{OpId, Sequence};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceRegion {
    leading_start: usize,
    leading_end: usize,
    body_start: usize,
    body_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DocumentSource {
    original: String,
    preamble_end: usize,
    trailer_start: usize,
    order: Vec<BlockId>,
    regions: BTreeMap<BlockId, SourceRegion>,
    #[serde(skip)]
    root_by_block: BTreeMap<BlockId, BlockId>,
    #[serde(skip)]
    root_by_elem: BTreeMap<OpId, BlockId>,
    dirty: BTreeSet<BlockId>,
}

impl DocumentSource {
    pub(crate) fn new(
        original: String,
        spans: Vec<(BlockId, usize, usize)>,
        blocks: &[Block],
    ) -> Self {
        let preamble_end = spans.first().map_or(original.len(), |(_, start, _)| *start);
        let trailer_start = spans.last().map_or(original.len(), |(_, _, end)| *end);
        let mut previous_end = preamble_end;
        let mut regions = BTreeMap::new();
        let mut order = Vec::with_capacity(spans.len());
        for (id, start, end) in spans {
            regions.insert(
                id,
                SourceRegion {
                    leading_start: previous_end,
                    leading_end: start,
                    body_start: start,
                    body_end: end,
                },
            );
            order.push(id);
            previous_end = end;
        }
        let mut source = Self {
            original,
            preamble_end,
            trailer_start,
            order,
            regions,
            root_by_block: BTreeMap::new(),
            root_by_elem: BTreeMap::new(),
            dirty: BTreeSet::new(),
        };
        source.index_roots(blocks);
        source
    }

    pub(crate) fn adopt_for(&self, blocks: &[&Block]) -> Option<Self> {
        if blocks.len() != self.order.len() {
            return None;
        }
        let mut regions = BTreeMap::new();
        let mut order = Vec::with_capacity(blocks.len());
        for (block, old_id) in blocks.iter().zip(&self.order) {
            regions.insert(block.id, self.regions.get(old_id)?.clone());
            order.push(block.id);
        }
        let mut source = Self {
            original: self.original.clone(),
            preamble_end: self.preamble_end,
            trailer_start: self.trailer_start,
            order,
            regions,
            root_by_block: BTreeMap::new(),
            root_by_elem: BTreeMap::new(),
            dirty: BTreeSet::new(),
        };
        for block in blocks {
            index_block(
                block,
                block.id,
                &mut source.root_by_block,
                &mut source.root_by_elem,
            );
        }
        Some(source)
    }

    pub(crate) fn mark_block_dirty(&mut self, block_id: BlockId) {
        if let Some(root) = self.root_by_block.get(&block_id) {
            self.dirty.insert(*root);
        }
    }

    pub(crate) fn reindex(&mut self, blocks: &Sequence<Block>) {
        self.root_by_block.clear();
        self.root_by_elem.clear();
        for block in blocks.iter_asc() {
            index_block(
                block,
                block.id,
                &mut self.root_by_block,
                &mut self.root_by_elem,
            );
        }
    }

    pub(crate) fn mark_elem_dirty(&mut self, elem_id: OpId) {
        if let Some(root) = self.root_by_elem.get(&elem_id) {
            self.dirty.insert(*root);
        }
    }

    pub(crate) fn render(&self, blocks: &Sequence<Block>) -> String {
        let mut output = self.original[..self.preamble_end].to_string();
        let mut emitted_block = false;
        let mut previous_source_position = None;
        for block in blocks.iter_asc() {
            if let Some(region) = self.regions.get(&block.id) {
                let source_position = self.order.iter().position(|id| *id == block.id);
                if (!emitted_block && source_position == Some(0))
                    || previous_source_position
                        .zip(source_position)
                        .is_some_and(|(previous, current)| previous + 1 == current)
                {
                    output.push_str(&self.original[region.leading_start..region.leading_end]);
                } else if emitted_block || !output.is_empty() {
                    ensure_block_separator(&mut output);
                }
                if self.dirty.contains(&block.id) {
                    output.push_str(&super::serialize::serialize_block(block));
                } else {
                    output.push_str(&self.original[region.body_start..region.body_end]);
                }
                previous_source_position = source_position;
            } else {
                if emitted_block || !output.is_empty() {
                    ensure_block_separator(&mut output);
                }
                output.push_str(&super::serialize::serialize_block(block));
                previous_source_position = None;
            }
            emitted_block = true;
        }
        if emitted_block || !output.is_empty() {
            output.push_str(&self.original[self.trailer_start..]);
        }
        output
    }

    fn index_roots(&mut self, blocks: &[Block]) {
        for block in blocks {
            index_block(
                block,
                block.id,
                &mut self.root_by_block,
                &mut self.root_by_elem,
            );
        }
    }
}

fn ensure_block_separator(output: &mut String) {
    if !output.ends_with("\n\n") {
        if !output.ends_with('\n') {
            output.push('\n');
        }
        output.push('\n');
    }
}

fn index_block(
    block: &Block,
    root: BlockId,
    by_block: &mut BTreeMap<BlockId, BlockId>,
    by_elem: &mut BTreeMap<OpId, BlockId>,
) {
    by_block.insert(block.id, root);
    by_elem.insert(block.elem_id, root);
    match &block.kind {
        BlockKind::BlockQuote { children } => {
            for child in children.iter_asc() {
                index_block(child, root, by_block, by_elem);
            }
        }
        BlockKind::List { items, .. } => {
            for item in items.iter_asc() {
                by_block.insert(item.id, root);
                by_elem.insert(item.elem_id, root);
                for child in item.children.iter_asc() {
                    index_block(child, root, by_block, by_elem);
                }
            }
        }
        _ => {}
    }
}
