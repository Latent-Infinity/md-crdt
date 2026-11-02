use super::frontmatter::FrontmatterRevision;
use super::{Block, BlockId, BlockKind, Frontmatter};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DocumentSource {
    original: String,
    preamble_end: usize,
    trailer_start: usize,
    order: Vec<BlockId>,
    regions: BTreeMap<BlockId, SourceRegion>,
    #[serde(skip)]
    ordinal_by_block: BTreeMap<BlockId, usize>,
    #[serde(skip)]
    root_by_block: BTreeMap<BlockId, BlockId>,
    #[serde(skip)]
    root_by_elem: BTreeMap<OpId, BlockId>,
    dirty: BTreeSet<BlockId>,
    #[serde(deserialize_with = "Option::deserialize")]
    frontmatter_revision: Option<FrontmatterRevision>,
}

impl PartialEq for DocumentSource {
    fn eq(&self, other: &Self) -> bool {
        // Restore rebuilds lookup accelerators; they are not document state.
        self.original == other.original
            && self.preamble_end == other.preamble_end
            && self.trailer_start == other.trailer_start
            && self.order == other.order
            && self.regions == other.regions
            && self.dirty == other.dirty
            && self.frontmatter_revision == other.frontmatter_revision
    }
}

impl Eq for DocumentSource {}

impl DocumentSource {
    pub(crate) fn record_frontmatter(
        &mut self,
        frontmatter: Option<&Frontmatter>,
        source_structured: bool,
    ) {
        self.frontmatter_revision =
            frontmatter.map(|frontmatter| frontmatter.source_revision(source_structured));
    }

    pub(crate) fn frontmatter_matches(&self, frontmatter: Option<&Frontmatter>) -> bool {
        match (&self.frontmatter_revision, frontmatter) {
            (Some(revision), Some(frontmatter)) => frontmatter.matches_source_revision(revision),
            (None, None) => true,
            _ => false,
        }
    }

    #[cfg(feature = "filesync")]
    pub(crate) fn frontmatter_is_structured(&self) -> bool {
        self.frontmatter_revision
            .as_ref()
            .is_none_or(|revision| revision.source_structured)
    }

    pub(crate) fn blocks_frontmatter_edit(&self, frontmatter: &Frontmatter) -> bool {
        self.frontmatter_revision.as_ref().is_some_and(|revision| {
            !revision.source_structured && frontmatter.matches_source_base(revision)
        })
    }

    pub(crate) fn frontmatter_replacement(&self, frontmatter: &Frontmatter) -> Option<String> {
        if self.frontmatter_matches(Some(frontmatter)) || self.blocks_frontmatter_edit(frontmatter)
        {
            return None;
        }
        if frontmatter.is_structured()
            && self.frontmatter_revision.as_ref().is_some_and(|revision| {
                revision.source_structured && frontmatter.matches_source_base(revision)
            })
            && let Some(raw) = self.frontmatter_content()
        {
            // A field write changes values, not adopted source comments/layout.
            return Some(frontmatter.render_on(raw));
        }
        Some(frontmatter.render())
    }

    fn frontmatter_content(&self) -> Option<&str> {
        let preamble = &self.original[..self.preamble_end];
        let first_line_end = preamble.find('\n').map_or(preamble.len(), |at| at + 1);
        if preamble[..first_line_end].trim() != "---" {
            return None;
        }
        let mut closing = first_line_end;
        for line in preamble[first_line_end..].split_inclusive('\n') {
            if line.trim() == "---" {
                return Some(&preamble[first_line_end..closing]);
            }
            closing += line.len();
        }
        None
    }

    /// Validate every persisted offset before it can be used for string slicing.
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        let boundary = |at| self.original.is_char_boundary(at);
        if !boundary(self.preamble_end)
            || !boundary(self.trailer_start)
            || self.preamble_end > self.trailer_start
        {
            return Err("invalid source preamble or trailer");
        }
        if self.order.len() != self.regions.len() {
            return Err("source order and regions differ");
        }
        let mut seen = BTreeSet::new();
        let mut previous_end = self.preamble_end;
        for id in &self.order {
            if !seen.insert(*id) {
                return Err("duplicate source block");
            }
            let region = self.regions.get(id).ok_or("missing source region")?;
            if region.leading_start != previous_end
                || region.leading_end != region.body_start
                || region.leading_start > region.leading_end
                || region.body_start > region.body_end
                || !boundary(region.leading_start)
                || !boundary(region.leading_end)
                || !boundary(region.body_start)
                || !boundary(region.body_end)
            {
                return Err("invalid source region bounds");
            }
            previous_end = region.body_end;
        }
        if previous_end != self.trailer_start {
            return Err("source trailer does not follow regions");
        }
        Ok(())
    }

    fn reindex_ordinals(&mut self) {
        self.ordinal_by_block = self
            .order
            .iter()
            .copied()
            .enumerate()
            .map(|(ordinal, id)| (id, ordinal))
            .collect();
    }

    #[cfg(feature = "filesync")]
    pub(crate) fn from_rendered(
        original: String,
        spans: Vec<(BlockId, usize, usize)>,
        blocks: &Sequence<Block>,
    ) -> Self {
        let mut source = Self::new(original, spans, &[]);
        source.reindex(blocks);
        source
    }

    pub(crate) fn region_body_bytes(&self, block_id: BlockId) -> Option<usize> {
        self.regions
            .get(&block_id)
            .map(|region| region.body_end.saturating_sub(region.body_start))
    }

    #[cfg(feature = "filesync")]
    pub(crate) fn render_root_region(&self, root: BlockId, block: &Block) -> Option<String> {
        let region = self.regions.get(&root)?;
        if self.dirty.contains(&root) {
            Some(super::serialize::serialize_block(block))
        } else {
            Some(self.original[region.body_start..region.body_end].to_string())
        }
    }

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
            ordinal_by_block: BTreeMap::new(),
            root_by_block: BTreeMap::new(),
            root_by_elem: BTreeMap::new(),
            dirty: BTreeSet::new(),
            frontmatter_revision: None,
        };
        source.reindex_ordinals();
        source.index_roots(blocks);
        source
    }

    pub(crate) fn adopt_for(&self, blocks: &[&Block]) -> Option<Self> {
        self.validate().ok()?;
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
            ordinal_by_block: BTreeMap::new(),
            root_by_block: BTreeMap::new(),
            root_by_elem: BTreeMap::new(),
            dirty: BTreeSet::new(),
            frontmatter_revision: self.frontmatter_revision.clone(),
        };
        source.reindex_ordinals();
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

    pub(crate) fn reindex(&mut self, blocks: &Sequence<Block>) {
        self.reindex_ordinals();
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

    pub(crate) fn mark_block_dirty(&mut self, block_id: BlockId) {
        if let Some(root) = self.root_by_block.get(&block_id) {
            self.dirty.insert(*root);
        }
    }

    pub(crate) fn mark_root_dirty(&mut self, root: BlockId) {
        self.dirty.insert(root);
    }

    pub(crate) fn mark_elem_dirty(&mut self, elem_id: OpId) {
        if let Some(root) = self.root_by_elem.get(&elem_id) {
            self.dirty.insert(*root);
        }
    }

    pub(crate) fn render_with_frontmatter(
        &self,
        blocks: &Sequence<Block>,
        frontmatter: Option<&str>,
    ) -> String {
        self.render_recorded(blocks, frontmatter, |_, _, _| {})
    }

    #[cfg(feature = "filesync")]
    pub(crate) fn render_with_ranges(
        &self,
        blocks: &Sequence<Block>,
        frontmatter: Option<&str>,
    ) -> (String, Vec<(BlockId, usize, usize)>) {
        let mut spans = Vec::with_capacity(self.order.len());
        let output = self.render_recorded(blocks, frontmatter, |id, start, end| {
            spans.push((id, start, end));
        });
        (output, spans)
    }

    fn region_line_ending(&self, region: &SourceRegion) -> &str {
        leading_line_ending(&self.original[region.body_end..])
    }

    fn render_recorded(
        &self,
        blocks: &Sequence<Block>,
        frontmatter: Option<&str>,
        mut record: impl FnMut(BlockId, usize, usize),
    ) -> String {
        let mut output = frontmatter.map_or_else(
            || self.original[..self.preamble_end].to_string(),
            |frontmatter| self.render_replaced_frontmatter(frontmatter),
        );
        let mut emitted_block = false;
        let mut previous_source_position = None;
        let mut previous_line_ending = "\n";
        for block in blocks.iter_asc() {
            let body_start;
            if let Some(region) = self.regions.get(&block.id) {
                let source_position = self.ordinal_by_block.get(&block.id).copied();
                if (!emitted_block && source_position == Some(0))
                    || previous_source_position
                        .zip(source_position)
                        .is_some_and(|(previous, current)| previous + 1 == current)
                {
                    let leading = &self.original[region.leading_start..region.leading_end];
                    // Only a preceding source region has already emitted the
                    // first newline in this trivia. The first region has no owner
                    // before it and must retain its complete leading bytes.
                    let owned_ending = if emitted_block {
                        leading_line_ending(leading).len()
                    } else {
                        0
                    };
                    output.push_str(&leading[owned_ending..]);
                } else if emitted_block || !output.is_empty() {
                    ensure_block_separator(&mut output, previous_line_ending);
                }
                body_start = output.len();
                if self.dirty.contains(&block.id) {
                    output.push_str(&super::serialize::serialize_block(block));
                } else {
                    output.push_str(&self.original[region.body_start..region.body_end]);
                }
                record(block.id, body_start, output.len());
                let ending = self.region_line_ending(region);
                output.push_str(ending);
                previous_line_ending = if ending.is_empty() { "\n" } else { ending };
                previous_source_position = source_position;
            } else {
                if emitted_block || !output.is_empty() {
                    ensure_block_separator(&mut output, previous_line_ending);
                }
                body_start = output.len();
                output.push_str(&super::serialize::serialize_block(block));
                record(block.id, body_start, output.len());
                previous_line_ending = "\n";
                previous_source_position = None;
            }
            emitted_block = true;
        }
        if emitted_block || !output.is_empty() {
            let trailer = &self.original[self.trailer_start..];
            // A retained region already emitted its own terminator, replacing
            // the original last region's terminator. A new canonical last block
            // has no source terminator, so it still inherits the whole trailer.
            // Keep all remaining trailer trivia, including trailing blank lines.
            let owned_ending = if previous_source_position.is_some() {
                leading_line_ending(trailer).len()
            } else {
                0
            };
            output.push_str(&trailer[owned_ending..]);
        }
        output
    }

    fn render_replaced_frontmatter(&self, frontmatter: &str) -> String {
        let preamble = &self.original[..self.preamble_end];
        let first_line_end = preamble.find('\n').map_or(preamble.len(), |at| at + 1);
        if preamble[..first_line_end].trim() == "---" {
            let mut closing = first_line_end;
            for line in preamble[first_line_end..].split_inclusive('\n') {
                if line.trim() == "---" {
                    let original = preamble[first_line_end..closing]
                        .strip_suffix('\n')
                        .unwrap_or(&preamble[first_line_end..closing]);
                    let original = original.strip_suffix('\r').unwrap_or(original);
                    if original
                        .split('\n')
                        .map(|line| line.strip_suffix('\r').unwrap_or(line))
                        .eq(frontmatter.split('\n'))
                    {
                        // Register versions do not imply a changed source projection.
                        return preamble.to_string();
                    }
                    let newline = if preamble[..first_line_end].ends_with("\r\n") {
                        "\r\n"
                    } else {
                        "\n"
                    };
                    let mut output = preamble[..first_line_end].to_string();
                    for (index, line) in frontmatter.split('\n').enumerate() {
                        if index != 0 {
                            output.push_str(newline);
                        }
                        output.push_str(line);
                    }
                    output.push_str(newline);
                    output.push_str(&preamble[closing..]);
                    return output;
                }
                closing += line.len();
            }
        }
        let mut output = format!("---\n{frontmatter}\n---\n");
        output.push_str(preamble);
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

fn leading_line_ending(text: &str) -> &str {
    if text.starts_with("\r\n") {
        &text[..2]
    } else if text.starts_with('\n') {
        &text[..1]
    } else {
        ""
    }
}

fn ensure_block_separator(output: &mut String, newline: &str) {
    if !output.ends_with("\n\n") && !output.ends_with("\n\r\n") {
        if !output.ends_with('\n') {
            output.push_str(newline);
        }
        output.push_str(newline);
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

#[cfg(test)]
mod council_regressions {
    use super::*;
    use crate::core::StateVector;
    use crate::doc::{EquivalenceMode, Parser, PendingListItemMove};

    #[test]
    fn source_validation_rejects_reversed_overlapping_and_duplicate_regions() {
        let document = Parser::parse("éclair\n\nsecond\n");
        let valid = document.source_state().unwrap();
        assert_eq!(valid.validate(), Ok(()));
        let mut invalid = valid.clone();
        invalid.order[1] = invalid.order[0];
        assert!(invalid.validate().is_err());
        let first = valid.order[0];
        for (start, end) in [(1, 2), (7, 3), (0, usize::MAX)] {
            let mut invalid = valid.clone();
            let region = invalid.regions.get_mut(&first).unwrap();
            region.body_start = start;
            region.body_end = end;
            assert!(invalid.validate().is_err());
        }
        let mut invalid = valid.clone();
        invalid
            .regions
            .get_mut(&valid.order[1])
            .unwrap()
            .leading_start = 0;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn document_application_refuses_descendant_moves_without_deleting_source() {
        let mut document = Parser::parse("- outer\n  - inner\n");
        let outer = document.blocks_in_order()[0];
        let BlockKind::List { items, .. } = &outer.kind else {
            panic!("list")
        };
        let item = items.iter().next().unwrap();
        let nested = item
            .children
            .iter()
            .find(|block| matches!(block.kind, BlockKind::List { .. }))
            .unwrap();
        let movement = PendingListItemMove {
            from_list_elem: outer.elem_id,
            to_list_elem: nested.elem_id,
            list_id: nested.id,
            item_id: item.id,
            target: item.elem_id,
            id: OpId {
                peer: 42,
                counter: 1,
            },
            after: None,
            right_origin: None,
            observed: StateVector::new(),
        };
        let before = document.clone();
        assert!(!document.move_list_item_at(&movement));
        assert_eq!(document, before);
        assert_eq!(
            document.serialize(EquivalenceMode::Exact),
            "- outer\n  - inner\n"
        );
    }

    #[test]
    fn moved_neighbors_keep_each_unrelated_regions_own_line_ending() {
        use crate::CollaborativeDocument;
        use crate::session::snapshot::DocumentDto;
        use crate::sync::{CheckpointRequest, DocumentTombstonePolicy};

        for (markdown, quote_first, expected) in [
            (
                "> original **bold**\n\n#  Untouched ##\r\n",
                false,
                "#  Untouched ##\r\n\r\n> original **bold**\n",
            ),
            (
                "#  Untouched ##\r\n\r\n> original **bold**\n",
                true,
                "> original **bold**\n\n#  Untouched ##\r\n",
            ),
        ] {
            let mut snapshot = CollaborativeDocument::new(1).save_snapshot().unwrap();
            snapshot.document = DocumentDto::from_document(&Parser::parse(markdown));
            let mut session = CollaborativeDocument::rebase_from_snapshot(snapshot, 2).unwrap();
            let quote = session
                .document()
                .blocks()
                .iter()
                .find(|block| matches!(block.kind, BlockKind::BlockQuote { .. }))
                .unwrap()
                .id;
            let heading = session
                .document()
                .blocks()
                .iter()
                .find(|block| matches!(block.kind, BlockKind::Heading { .. }))
                .unwrap();
            let heading_id = heading.id;
            let heading_elem = heading.elem_id;
            session
                .move_block(quote, None, (!quote_first).then_some(heading_elem))
                .unwrap();
            assert_eq!(
                session.document().serialize(EquivalenceMode::Exact),
                expected
            );
            assert_eq!(
                session.document().block_elem_id(heading_id),
                Some(heading_elem)
            );
            session
                .checkpoint_history(&CheckpointRequest {
                    max_retained_ops: 0,
                    active_peer_leases: Vec::new(),
                    tombstones: DocumentTombstonePolicy::KeepAll,
                })
                .unwrap();
            let bytes = session.save_snapshot().unwrap().to_bytes().unwrap();
            let mut restored = CollaborativeDocument::restore_from_snapshot(
                crate::SessionSnapshot::from_bytes(&bytes).unwrap(),
            )
            .unwrap();
            assert_eq!(
                restored.document().serialize(EquivalenceMode::Exact),
                expected
            );
            assert_eq!(restored.document(), session.document());
            #[cfg(feature = "filesync")]
            {
                restored.document_mut().refresh_source_metadata();
                assert_eq!(
                    restored.document().serialize(EquivalenceMode::Exact),
                    expected
                );
                assert_eq!(restored.document().blocks, session.document().blocks);
            }
            // A later edit must still invalidate the heading's retained source.
            restored.insert_text(heading_id, 0, "changed ").unwrap();
            let edited = restored.document().serialize(EquivalenceMode::Exact);
            assert!(edited.contains("# changed Untouched\r\n"));
            assert!(edited.contains("**bold**"));
        }
    }

    #[cfg(feature = "filesync")]
    #[test]
    fn source_refresh_preserves_rendered_trivia_and_following_edits() {
        let mut document =
            Parser::parse("---\r\ntitle: old\r\n---\r\n\r\n#  Styled ##\r\n\r\nalpha beta\r\n");
        let paragraph = document.blocks_in_order()[1].id;
        document
            .insert_text(
                paragraph,
                6,
                "brave ",
                OpId {
                    peer: 42,
                    counter: 1,
                },
            )
            .unwrap();
        document
            .set_frontmatter_field(
                "title".into(),
                Some("new".into()),
                OpId {
                    peer: 42,
                    counter: 20,
                },
            )
            .unwrap();
        let expected = document.serialize(EquivalenceMode::Exact);
        let before = document.blocks.clone();
        document.refresh_source_metadata();
        assert_eq!(document.source_state().unwrap().validate(), Ok(()));
        assert_eq!(document.serialize(EquivalenceMode::Exact), expected);
        assert_eq!(document.blocks, before);
        document
            .insert_text(
                paragraph,
                0,
                "new ",
                OpId {
                    peer: 42,
                    counter: 30,
                },
            )
            .unwrap();
        assert_eq!(
            document.serialize(EquivalenceMode::Exact),
            expected.replace("alpha brave beta", "new alpha brave beta")
        );
        let expected = document.serialize(EquivalenceMode::Exact);
        document.refresh_source_metadata();
        assert_eq!(document.serialize(EquivalenceMode::Exact), expected);
    }
}
