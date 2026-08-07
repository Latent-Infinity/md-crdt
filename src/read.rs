//! Read-only Markdown queries that never instantiate the collaborative session layer.
//!
//! [`ReadDocument`] parses Markdown once and answers outline, section, search and
//! link questions from the parsed [`Document`]. Parsing is pure: it performs no
//! file system access, consults no clock, draws no randomness, allocates no peer
//! id, and writes nothing — no `.mdcrdt` sidecar, no ingest, no storage. It is
//! the right entry point for consumers that only read.
//!
//! # Blocks are addressed by ordinal, never by id
//!
//! [`crate::doc::Parser::parse`] seeds every `OpId` with peer `0`, and peer `0` is
//! reserved: a `VaultSession` rejects it. The `BlockId`s inside
//! a parsed document are therefore **not** valid session addresses and this module
//! deliberately does not expose them — treating one as a mutable handle would
//! address a block that no session can own. Every block in this API is identified
//! by its *ordinal*: the zero-based index into
//! [`Document::blocks_in_order`], which enumerates top-level blocks only.
//! An ordinal is meaningful only for the exact Markdown text it was parsed from;
//! re-parse after any edit, and never persist one.
//!
//! # Memory
//!
//! Parsing allocates roughly 140x the source size, because every grapheme cluster
//! of paragraph and heading text becomes its own CRDT unit. Query one document at
//! a time and drop it; do not hold many [`ReadDocument`]s alive at once.

use crate::core::mark::{MarkInterval, MarkKind, MarkValue};
use crate::doc::{Block, BlockKind, Document, Parser, Table, paragraph_visible_string};

/// Characters of context kept on either side of a search match.
const SNIPPET_RADIUS: usize = 48;

/// A parsed Markdown document that can only be queried, never edited.
///
/// See the [module documentation](self) for the ordinal addressing contract and
/// the memory cost of parsing.
#[derive(Debug)]
pub struct ReadDocument {
    document: Document,
}

/// One heading in [`ReadDocument::outline`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineEntry {
    /// Index of the heading block in document order.
    pub ordinal: usize,
    /// Heading depth, 1–6.
    pub level: u8,
    /// Visible heading text with inline Markdown delimiters removed.
    pub title: String,
}

/// One block returned by [`ReadDocument::section`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadBlock {
    /// Index of the block in document order.
    pub ordinal: usize,
    /// Structural kind of the block.
    pub kind: ReadBlockKind,
    /// Visible text of the block, including nested children.
    pub text: String,
}

/// One match returned by [`ReadDocument::search`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Index of the matching block in document order.
    pub ordinal: usize,
    /// Bounded window of visible text around the first match in the block.
    pub snippet: String,
    /// Titles of the headings enclosing the block, outermost first.
    ///
    /// A heading block does not include its own title.
    pub heading_path: Vec<String>,
}

/// One outbound link returned by [`ReadDocument::links`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadLink {
    /// Index of the top-level block that owns the link, in document order.
    ///
    /// Links nested inside a list item or block quote report the ordinal of the
    /// enclosing top-level list or quote.
    pub ordinal: usize,
    /// Link destination, taken verbatim from the `href` attribute.
    pub target: String,
    /// Visible link label with inline Markdown delimiters removed.
    pub label: String,
}

/// Structural kind of a [`ReadBlock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadBlockKind {
    /// Paragraph text.
    Paragraph,
    /// ATX or setext heading.
    Heading,
    /// Fenced code block.
    CodeFence,
    /// Ordered or unordered list.
    List,
    /// One item of a list.
    ///
    /// Mirrors [`crate::workspace::BlockDescriptorKind`]. Because
    /// [`ReadDocument::section`] reports top-level blocks only, and a list item is
    /// never top-level, this variant is not produced today; it exists so the two
    /// taxonomies stay aligned.
    ListItem,
    /// Block quote.
    BlockQuote,
    /// GitHub-style table.
    Table,
    /// Verbatim block the parser does not model structurally.
    RawBlock,
}

impl ReadDocument {
    /// Parse Markdown for read-only querying.
    ///
    /// Pure: touches no file system, no clock, and no peer registry.
    pub fn parse(markdown: &str) -> Self {
        Self {
            document: Parser::parse(markdown),
        }
    }

    /// Borrow the parsed document.
    ///
    /// Lets a consumer reuse its own block traversal and addressing instead of
    /// the ordinals used here. The two address spaces differ: [`Self::outline`]
    /// and [`Self::section`] number only top-level blocks, via
    /// [`Document::blocks_in_order`], whereas a consumer that flattens nested
    /// list items and block-quote children numbers them differently. A
    /// consumer that already has such a traversal should build on this borrow
    /// rather than translate ordinals between the two.
    ///
    /// Block identities reachable through this borrow carry peer 0 and are not
    /// session addresses; see the module documentation.
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// Headings in document order.
    ///
    /// Fence-aware: a `#` line inside a fenced code block is code, so it never
    /// appears here.
    pub fn outline(&self) -> Vec<OutlineEntry> {
        self.document
            .blocks_in_order()
            .into_iter()
            .enumerate()
            .filter_map(|(ordinal, block)| match &block.kind {
                BlockKind::Heading { level, text } => Some(OutlineEntry {
                    ordinal,
                    level: *level,
                    title: paragraph_visible_string(text),
                }),
                _ => None,
            })
            .collect()
    }

    /// Blocks belonging to the heading at `ordinal`, up to the next heading of
    /// equal or shallower level.
    ///
    /// The heading itself is the first entry. Returns an empty vector when
    /// `ordinal` is out of range or does not address a heading.
    pub fn section(&self, ordinal: usize) -> Vec<ReadBlock> {
        let blocks = self.document.blocks_in_order();
        let Some(heading) = blocks.get(ordinal) else {
            return Vec::new();
        };
        let BlockKind::Heading { level, .. } = heading.kind else {
            return Vec::new();
        };
        let end = blocks[ordinal + 1..]
            .iter()
            .position(
                |block| matches!(block.kind, BlockKind::Heading { level: next, .. } if next <= level),
            )
            .map_or(blocks.len(), |relative| ordinal + 1 + relative);
        blocks[ordinal..end]
            .iter()
            .enumerate()
            .map(|(offset, block)| ReadBlock {
                ordinal: ordinal + offset,
                kind: read_block_kind(&block.kind),
                text: block_visible_text(block),
            })
            .collect()
    }

    /// Case-insensitive search over visible block text, bounded by `limit`.
    ///
    /// At most one hit is reported per block — the first match in it — and at most
    /// `limit` hits overall. An empty `needle` or a `limit` of zero matches nothing.
    /// Matching folds both sides to lowercase, so it is case-insensitive for
    /// scripts with simple case mappings.
    pub fn search(&self, needle: &str, limit: usize) -> Vec<SearchHit> {
        let blocks = self.document.blocks_in_order();
        if needle.is_empty() || limit == 0 || blocks.is_empty() {
            return Vec::new();
        }
        let folded_needle = needle.to_lowercase();
        let mut hits = Vec::with_capacity(limit.min(blocks.len()));
        let mut path: Vec<(u8, String)> = Vec::new();
        for (ordinal, block) in blocks.into_iter().enumerate() {
            let text = block_visible_text(block);
            if let Some((start, end)) = find_folded(&text, &folded_needle) {
                hits.push(SearchHit {
                    ordinal,
                    snippet: snippet(&text, start, end),
                    heading_path: path.iter().map(|(_, title)| title.clone()).collect(),
                });
                if hits.len() == limit {
                    break;
                }
            }
            if let BlockKind::Heading { level, text } = &block.kind {
                path.retain(|(enclosing, _)| *enclosing < *level);
                path.push((*level, paragraph_visible_string(text)));
            }
        }
        hits
    }

    /// Outbound links in document order.
    ///
    /// Only inline `[label](target)` links carry a link mark. `[[wikilinks]]` are
    /// not parsed as links — they stay literal text — so they are never reported
    /// here.
    pub fn links(&self) -> Vec<ReadLink> {
        let mut links = Vec::new();
        for (ordinal, block) in self.document.blocks_in_order().into_iter().enumerate() {
            collect_links(block, ordinal, &mut links);
        }
        links
    }
}

fn read_block_kind(kind: &BlockKind) -> ReadBlockKind {
    match kind {
        BlockKind::Paragraph { .. } => ReadBlockKind::Paragraph,
        BlockKind::Heading { .. } => ReadBlockKind::Heading,
        BlockKind::CodeFence { .. } => ReadBlockKind::CodeFence,
        BlockKind::List { .. } => ReadBlockKind::List,
        BlockKind::BlockQuote { .. } => ReadBlockKind::BlockQuote,
        BlockKind::Table { .. } => ReadBlockKind::Table,
        BlockKind::RawBlock { .. } => ReadBlockKind::RawBlock,
    }
}

/// Visible text of a block, recursing into block quote and list children.
fn block_visible_text(block: &Block) -> String {
    match &block.kind {
        BlockKind::Paragraph { text } | BlockKind::Heading { text, .. } => {
            paragraph_visible_string(text)
        }
        BlockKind::CodeFence { text, .. } => text.clone(),
        BlockKind::RawBlock { raw } => raw.clone(),
        BlockKind::Table { table } => table_visible_text(table),
        BlockKind::BlockQuote { children } => join_visible_text(children.iter()),
        BlockKind::List { items, .. } => {
            let lines: Vec<String> = items
                .iter()
                .map(|item| join_visible_text(item.children.iter()))
                .collect();
            lines.join("\n")
        }
    }
}

fn join_visible_text<'a>(blocks: impl Iterator<Item = &'a Block>) -> String {
    blocks
        .map(block_visible_text)
        .collect::<Vec<String>>()
        .join("\n")
}

fn table_visible_text(table: &Table) -> String {
    let body = table.rows_in_order();
    let mut rows = Vec::with_capacity(body.len() + 1);
    rows.push(table.row_cells(table.header_row_id()).join(" | "));
    for row in &body {
        rows.push(table.row_cells(row.id).join(" | "));
    }
    rows.join("\n")
}

fn collect_links(block: &Block, ordinal: usize, out: &mut Vec<ReadLink>) {
    match &block.kind {
        BlockKind::Paragraph { text } | BlockKind::Heading { text, .. } => {
            let graphemes: Vec<&str> = text.iter().map(|unit| unit.grapheme.as_str()).collect();
            let mut resolved: Vec<_> = block
                .marks
                .resolved_intervals_in_sequence(text)
                .into_iter()
                .filter(|(interval, _, _)| interval.kind == MarkKind::Link)
                .collect();
            resolved.sort_by_key(|(interval, start, end)| (*start, *end, interval.id));
            for (interval, start, end) in resolved {
                if start >= end || end > graphemes.len() {
                    continue;
                }
                let Some(target) = string_attr(interval, "href") else {
                    continue;
                };
                out.push(ReadLink {
                    ordinal,
                    target,
                    label: graphemes[start..end].concat(),
                });
            }
        }
        BlockKind::BlockQuote { children } => {
            for child in children.iter() {
                collect_links(child, ordinal, out);
            }
        }
        BlockKind::List { items, .. } => {
            for item in items.iter() {
                for child in item.children.iter() {
                    collect_links(child, ordinal, out);
                }
            }
        }
        BlockKind::CodeFence { .. } | BlockKind::RawBlock { .. } | BlockKind::Table { .. } => {}
    }
}

fn string_attr(interval: &MarkInterval, key: &str) -> Option<String> {
    match interval.attrs.get(key)?.get_ref() {
        MarkValue::String(value) => Some(value.clone()),
        MarkValue::Bool(_) => None,
    }
}

/// Byte range of the first case-insensitive occurrence of `folded_needle`.
fn find_folded(haystack: &str, folded_needle: &str) -> Option<(usize, usize)> {
    haystack.char_indices().find_map(|(start, _)| {
        folded_match_len(&haystack[start..], folded_needle).map(|len| (start, start + len))
    })
}

/// Byte length consumed in `rest` by a case-insensitive prefix match, if any.
fn folded_match_len(rest: &str, folded_needle: &str) -> Option<usize> {
    let mut wanted = folded_needle.chars();
    let mut expected = wanted.next()?;
    for (offset, candidate) in rest.char_indices() {
        for folded in candidate.to_lowercase() {
            if folded != expected {
                return None;
            }
            match wanted.next() {
                Some(next) => expected = next,
                None => return Some(offset + candidate.len_utf8()),
            }
        }
    }
    None
}

/// Bounded window of `text` around the byte range `start..end`, elided at each
/// truncated edge.
fn snippet(text: &str, start: usize, end: usize) -> String {
    let from = text[..start]
        .char_indices()
        .rev()
        .take(SNIPPET_RADIUS)
        .last()
        .map_or(start, |(offset, _)| offset);
    let to = text[end..]
        .char_indices()
        .take(SNIPPET_RADIUS)
        .last()
        .map_or(end, |(offset, candidate)| {
            end + offset + candidate.len_utf8()
        });
    let mut out = String::with_capacity(to - from + 8);
    if from > 0 {
        out.push('…');
    }
    out.push_str(&text[from..to]);
    if to < text.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FENCED: &str = concat!(
        "# Real Heading\n",
        "\n",
        "```rust\n",
        "# Not A Heading\n",
        "```\n",
        "\n",
        "## Second Heading\n",
    );

    const NESTED: &str = concat!(
        "# Alpha\n",
        "\n",
        "alpha body\n",
        "\n",
        "## Beta\n",
        "\n",
        "beta body\n",
        "\n",
        "### Gamma\n",
        "\n",
        "gamma body\n",
        "\n",
        "# Delta\n",
        "\n",
        "delta body\n",
    );

    fn titles(document: &ReadDocument) -> Vec<String> {
        document
            .outline()
            .into_iter()
            .map(|entry| entry.title)
            .collect()
    }

    #[test]
    fn outline_skips_headings_inside_code_fences() {
        let document = ReadDocument::parse(FENCED);

        assert_eq!(
            titles(&document),
            vec!["Real Heading".to_string(), "Second Heading".to_string()],
            "a `#` line inside a fence is code, not a heading"
        );
    }

    #[test]
    fn outline_reports_levels_and_document_order_ordinals() {
        let document = ReadDocument::parse(FENCED);
        let entries = document.outline();

        assert_eq!(entries[0].ordinal, 0);
        assert_eq!(entries[0].level, 1);
        // Ordinal 1 is the fence block, so the next heading is ordinal 2.
        assert_eq!(entries[1].ordinal, 2);
        assert_eq!(entries[1].level, 2);
    }

    #[test]
    fn outline_titles_use_visible_text_without_inline_delimiters() {
        let document = ReadDocument::parse("## **Bold** and `code` title\n");

        assert_eq!(titles(&document), vec!["Bold and code title".to_string()]);
    }

    #[test]
    fn section_stops_at_equal_or_shallower_heading() {
        let document = ReadDocument::parse(NESTED);
        let section = document.section(0);

        let texts: Vec<&str> = section.iter().map(|block| block.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "Alpha",
                "alpha body",
                "Beta",
                "beta body",
                "Gamma",
                "gamma body"
            ],
            "the `# Alpha` section ends at the next level-1 heading"
        );
        assert_eq!(section[0].ordinal, 0);
        assert_eq!(section[5].ordinal, 5);
    }

    #[test]
    fn section_of_nested_heading_covers_only_its_own_subtree() {
        let document = ReadDocument::parse(NESTED);

        let beta: Vec<String> = document
            .section(2)
            .into_iter()
            .map(|block| block.text)
            .collect();
        assert_eq!(beta, vec!["Beta", "beta body", "Gamma", "gamma body"]);

        let gamma: Vec<String> = document
            .section(4)
            .into_iter()
            .map(|block| block.text)
            .collect();
        assert_eq!(gamma, vec!["Gamma", "gamma body"]);
    }

    #[test]
    fn section_of_last_heading_runs_to_end_of_document() {
        let document = ReadDocument::parse(NESTED);
        let section = document.section(6);

        assert_eq!(section.len(), 2);
        assert_eq!(section[1].text, "delta body");
    }

    #[test]
    fn section_is_empty_for_non_heading_and_out_of_range_ordinals() {
        let document = ReadDocument::parse(NESTED);

        assert!(document.section(1).is_empty(), "ordinal 1 is a paragraph");
        assert!(document.section(999).is_empty(), "ordinal is out of range");
    }

    #[test]
    fn section_reports_block_kinds_and_visible_text_per_kind() {
        let markdown = concat!(
            "# Kinds\n",
            "\n",
            "plain paragraph\n",
            "\n",
            "```txt\n",
            "fence body\n",
            "```\n",
            "\n",
            "- first item\n",
            "- second item\n",
            "\n",
            "> quoted line\n",
            "\n",
            "| a | b |\n",
            "| --- | --- |\n",
            "| 1 | 2 |\n",
            "\n",
            ":::note\n",
            "raw body\n",
            ":::\n",
        );
        let document = ReadDocument::parse(markdown);
        let section = document.section(0);

        let kinds: Vec<ReadBlockKind> = section.iter().map(|block| block.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ReadBlockKind::Heading,
                ReadBlockKind::Paragraph,
                ReadBlockKind::CodeFence,
                ReadBlockKind::List,
                ReadBlockKind::BlockQuote,
                ReadBlockKind::Table,
                ReadBlockKind::RawBlock,
            ]
        );
        assert_eq!(section[2].text, "fence body");
        assert_eq!(section[3].text, "first item\nsecond item");
        assert_eq!(section[4].text, "quoted line");
        assert_eq!(section[5].text, "a | b\n1 | 2");
        assert!(section[6].text.contains("raw body"));
    }

    #[test]
    fn search_is_case_insensitive_and_reports_heading_path() {
        let markdown = concat!(
            "# Guide\n",
            "\n",
            "Alpha BETA gamma.\n",
            "\n",
            "## Details\n",
            "\n",
            "beta again here.\n",
        );
        let document = ReadDocument::parse(markdown);
        let hits = document.search("BeTa", 10);

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].ordinal, 1);
        assert!(hits[0].snippet.contains("BETA"));
        assert_eq!(hits[0].heading_path, vec!["Guide".to_string()]);
        assert_eq!(hits[1].ordinal, 3);
        assert_eq!(
            hits[1].heading_path,
            vec!["Guide".to_string(), "Details".to_string()]
        );
    }

    #[test]
    fn search_stops_at_limit_and_rejects_degenerate_input() {
        let markdown = "needle one\n\nneedle two\n\nneedle three\n";
        let document = ReadDocument::parse(markdown);

        assert_eq!(document.search("needle", 10).len(), 3);
        assert_eq!(document.search("needle", 2).len(), 2);
        assert!(document.search("needle", 0).is_empty());
        assert!(document.search("", 10).is_empty());
        assert!(document.search("absent", 10).is_empty());
    }

    #[test]
    fn search_snippet_is_bounded_and_elided() {
        let filler = "x".repeat(400);
        let markdown = format!("{filler} needle {filler}\n");
        let document = ReadDocument::parse(&markdown);
        let hits = document.search("needle", 1);

        assert_eq!(hits.len(), 1);
        let snippet = &hits[0].snippet;
        assert!(snippet.contains("needle"));
        assert!(
            snippet.chars().count() < markdown.chars().count(),
            "snippet must be a bounded window, not the whole block"
        );
        assert!(snippet.starts_with('…') && snippet.ends_with('…'));
    }

    #[test]
    fn search_matches_nested_and_fenced_block_text() {
        let markdown = concat!(
            "# Top\n",
            "\n",
            "```rust\n",
            "let needle = 1;\n",
            "```\n",
            "\n",
            "- item with needle\n",
        );
        let document = ReadDocument::parse(markdown);
        let hits = document.search("needle", 10);

        let ordinals: Vec<usize> = hits.iter().map(|hit| hit.ordinal).collect();
        assert_eq!(ordinals, vec![1, 2]);
    }

    #[test]
    fn links_report_target_label_and_owning_ordinal() {
        let markdown = concat!(
            "# Title\n",
            "\n",
            "See [Docs](https://example.com/docs) and [Local](./notes.md).\n",
        );
        let document = ReadDocument::parse(markdown);
        let links = document.links();

        assert_eq!(links.len(), 2);
        assert_eq!(links[0].ordinal, 1);
        assert_eq!(links[0].target, "https://example.com/docs");
        assert_eq!(links[0].label, "Docs");
        assert_eq!(links[1].ordinal, 1);
        assert_eq!(links[1].target, "./notes.md");
        assert_eq!(links[1].label, "Local");
    }

    #[test]
    fn links_inside_list_items_and_quotes_use_top_level_ordinal() {
        let markdown = concat!(
            "- plain item\n",
            "- item with [Nested](https://example.com/nested)\n",
            "\n",
            "> quoted [Quoted](https://example.com/quoted)\n",
        );
        let document = ReadDocument::parse(markdown);
        let links = document.links();

        assert_eq!(links.len(), 2);
        assert_eq!(links[0].ordinal, 0, "list block owns the nested link");
        assert_eq!(links[0].label, "Nested");
        assert_eq!(links[1].ordinal, 1, "blockquote block owns the quoted link");
        assert_eq!(links[1].target, "https://example.com/quoted");
    }

    #[test]
    fn links_include_heading_links_and_strip_inline_delimiters() {
        let document = ReadDocument::parse("## Go to [**There**](there.md)\n");
        let links = document.links();

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].ordinal, 0);
        assert_eq!(links[0].label, "There");
        assert_eq!(links[0].target, "there.md");
    }

    #[test]
    fn wikilinks_are_extracted_as_links() {
        let document = ReadDocument::parse("See [[Page Name]] and [[Other|Alias]] here.\n");
        let links = document.links();

        // A bare wikilink labels itself; an aliased one shows the alias and
        // targets the file.
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].label, "Page Name");
        assert_eq!(links[0].target, "Page Name");
        assert_eq!(links[1].label, "Alias");
        assert_eq!(links[1].target, "Other");
        assert_eq!(
            document.section(0),
            Vec::new(),
            "the wikilink paragraph is not a heading"
        );
    }

    #[test]
    fn empty_document_answers_every_query_with_nothing() {
        let document = ReadDocument::parse("");

        assert!(document.outline().is_empty());
        assert!(document.section(0).is_empty());
        assert!(document.search("anything", 10).is_empty());
        assert!(document.links().is_empty());
    }

    #[test]
    fn document_accessor_exposes_blocks_that_ordinals_do_not_address() {
        let markdown = concat!(
            "# Checklist\n",
            "\n",
            "- Deploy\n",
            "  - Roll canary\n",
            "  - Promote\n",
            "\n",
            "## After\n",
        );
        let document = ReadDocument::parse(markdown);

        // Ordinals number top-level blocks only, so the nested list items are
        // unreachable through them. A consumer that flattens nested blocks
        // must build on `document()` rather than translate ordinals, or the
        // two address spaces will silently disagree.
        let top_level = document.document().blocks_in_order();
        assert_eq!(
            top_level.len(),
            3,
            "heading, list, heading: the two nested items get no ordinal"
        );

        let nested_reachable = top_level.iter().any(|block| {
            matches!(&block.kind, crate::doc::BlockKind::List { items, .. }
                if items.len_visible() == 1)
        });
        assert!(
            nested_reachable,
            "the nested items stay reachable through the borrowed document"
        );

        let outline_ordinals: Vec<usize> = document
            .outline()
            .iter()
            .map(|entry| entry.ordinal)
            .collect();
        assert_eq!(outline_ordinals, vec![0, 2]);
    }

    #[test]
    fn frontmatter_does_not_consume_block_ordinals() {
        let markdown = concat!(
            "---\n",
            "title: Test Doc\n",
            "tags: a, b\n",
            "---\n",
            "\n",
            "# Heading\n",
            "\n",
            "body text\n",
        );
        let document = ReadDocument::parse(markdown);
        let entries = document.outline();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].ordinal, 0);
        assert_eq!(entries[0].title, "Heading");
        assert_eq!(document.section(0).len(), 2);
        assert!(
            document.search("title", 10).is_empty(),
            "frontmatter is not part of the searchable block text"
        );
    }
}
