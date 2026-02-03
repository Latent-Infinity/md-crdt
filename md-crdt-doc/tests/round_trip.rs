//! Comprehensive round-trip tests for the markdown parser.
//!
//! These tests verify that parse -> serialize -> parse -> serialize
//! produces stable, idempotent output for all supported block types
//! and edge cases.

use md_crdt_doc::{EquivalenceMode, Parser, SerializeConfig};

/// Helper to assert round-trip idempotency
fn assert_round_trip(input: &str, mode: EquivalenceMode) {
    let config = SerializeConfig {
        equivalence: mode,
        prefer_raw_source: mode == EquivalenceMode::Exact,
    };

    let doc1 = Parser::parse(input);
    let output1 = doc1.serialize_with_config(&config);
    let doc2 = Parser::parse(&output1);
    let output2 = doc2.serialize_with_config(&config);

    assert_eq!(
        output1, output2,
        "Round-trip not idempotent for input:\n{:?}\nFirst output:\n{:?}\nSecond output:\n{:?}",
        input, output1, output2
    );
}

fn assert_structural_round_trip(input: &str) {
    assert_round_trip(input, EquivalenceMode::Structural);
}

fn assert_exact_round_trip(input: &str) {
    assert_round_trip(input, EquivalenceMode::Exact);
}

// =============================================================================
// Basic Block Types
// =============================================================================

mod paragraphs {
    use super::*;

    #[test]
    fn single_paragraph() {
        assert_structural_round_trip("Hello, world!");
    }

    #[test]
    fn multiple_paragraphs() {
        assert_structural_round_trip("First paragraph.\n\nSecond paragraph.");
    }

    #[test]
    fn paragraph_with_soft_breaks() {
        assert_structural_round_trip("Line one\nLine two\nLine three");
    }

    #[test]
    fn empty_document() {
        assert_structural_round_trip("");
    }

    #[test]
    fn whitespace_only() {
        assert_structural_round_trip("   \n\n   \n");
    }
}

mod code_fences {
    use super::*;

    #[test]
    fn basic_code_fence() {
        assert_structural_round_trip("```\ncode here\n```");
    }

    #[test]
    fn code_fence_with_language() {
        assert_structural_round_trip("```rust\nfn main() {}\n```");
    }

    #[test]
    fn code_fence_with_special_chars() {
        assert_structural_round_trip("```\n<html>&amp;</html>\n```");
    }

    #[test]
    fn code_fence_empty() {
        assert_structural_round_trip("```\n```");
    }

    #[test]
    fn code_fence_with_backticks_inside() {
        assert_structural_round_trip("````\n```\ncode\n```\n````");
    }

    #[test]
    fn indented_code_block() {
        assert_structural_round_trip("    code line 1\n    code line 2");
    }
}

mod block_quotes {
    use super::*;

    #[test]
    fn single_level_quote() {
        assert_structural_round_trip("> This is a quote");
    }

    #[test]
    fn multi_line_quote() {
        assert_structural_round_trip("> Line one\n> Line two");
    }

    #[test]
    fn nested_quotes() {
        assert_structural_round_trip("> Level 1\n>> Level 2\n>>> Level 3");
    }

    #[test]
    fn quote_with_code() {
        assert_structural_round_trip("> Quote with `inline code`");
    }

    #[test]
    fn quote_with_paragraph_break() {
        assert_structural_round_trip("> First quote\n\n> Second quote");
    }
}

mod headings {
    use super::*;

    #[test]
    fn atx_headings_all_levels() {
        assert_structural_round_trip("# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6");
    }

    #[test]
    fn setext_heading_h1() {
        assert_structural_round_trip("Heading\n=======");
    }

    #[test]
    fn setext_heading_h2() {
        assert_structural_round_trip("Heading\n-------");
    }

    #[test]
    fn heading_with_inline_formatting() {
        assert_structural_round_trip("# Heading with **bold** and *italic*");
    }
}

mod lists {
    use super::*;

    #[test]
    fn unordered_list() {
        assert_structural_round_trip("- Item 1\n- Item 2\n- Item 3");
    }

    #[test]
    fn ordered_list() {
        assert_structural_round_trip("1. First\n2. Second\n3. Third");
    }

    #[test]
    fn nested_list() {
        assert_structural_round_trip("- Item 1\n  - Nested 1\n  - Nested 2\n- Item 2");
    }

    #[test]
    fn list_with_paragraphs() {
        assert_structural_round_trip("- Item 1\n\n  Paragraph in item\n\n- Item 2");
    }

    #[test]
    fn mixed_list_markers() {
        assert_structural_round_trip("* Star item\n- Dash item\n+ Plus item");
    }
}

mod tables {
    use super::*;

    #[test]
    fn basic_table() {
        assert_structural_round_trip("| A | B |\n|---|---|\n| 1 | 2 |");
    }

    #[test]
    fn table_with_alignment() {
        assert_structural_round_trip("| Left | Center | Right |\n|:-----|:------:|------:|\n| L | C | R |");
    }

    #[test]
    fn table_multiple_rows() {
        assert_structural_round_trip(
            "| H1 | H2 |\n|---|---|\n| A | B |\n| C | D |\n| E | F |",
        );
    }

    #[test]
    fn table_with_inline_code() {
        assert_structural_round_trip("| Code |\n|---|\n| `fn()` |");
    }
}

mod frontmatter {
    use super::*;

    #[test]
    fn yaml_frontmatter() {
        assert_exact_round_trip("---\ntitle: Test\nauthor: Me\n---\n\nContent here");
    }

    #[test]
    fn frontmatter_empty_content() {
        assert_exact_round_trip("---\nkey: value\n---\n");
    }

    #[test]
    fn frontmatter_complex_yaml() {
        assert_exact_round_trip("---\ntags:\n  - rust\n  - crdt\ncount: 42\n---\n\nBody");
    }
}

// =============================================================================
// Inline Formatting
// =============================================================================

mod inline_formatting {
    use super::*;

    #[test]
    fn bold() {
        assert_structural_round_trip("This is **bold** text");
    }

    #[test]
    fn italic() {
        assert_structural_round_trip("This is *italic* text");
    }

    #[test]
    fn bold_and_italic() {
        assert_structural_round_trip("This is ***bold and italic*** text");
    }

    #[test]
    fn inline_code() {
        assert_structural_round_trip("Use `code` inline");
    }

    #[test]
    fn strikethrough() {
        assert_structural_round_trip("This is ~~deleted~~ text");
    }

    #[test]
    fn links() {
        assert_structural_round_trip("Click [here](https://example.com)");
    }

    #[test]
    fn link_with_title() {
        assert_structural_round_trip("[link](url \"title\")");
    }

    #[test]
    fn images() {
        assert_structural_round_trip("![alt text](image.png)");
    }

    #[test]
    fn autolinks() {
        assert_structural_round_trip("<https://example.com>");
    }

    #[test]
    fn nested_formatting() {
        assert_structural_round_trip("**bold with *italic* inside**");
    }
}

// =============================================================================
// Edge Cases
// =============================================================================

mod edge_cases {
    use super::*;

    #[test]
    fn unicode_basic() {
        assert_structural_round_trip("Hello 世界 🌍");
    }

    #[test]
    fn unicode_rtl() {
        assert_structural_round_trip("مرحبا بالعالم");
    }

    #[test]
    fn unicode_combining_chars() {
        assert_structural_round_trip("café résumé naïve");
    }

    #[test]
    fn unicode_zero_width() {
        assert_structural_round_trip("zero\u{200B}width\u{200B}space");
    }

    #[test]
    fn emoji_sequences() {
        assert_structural_round_trip("👨‍👩‍👧‍👦 family emoji");
    }

    #[test]
    fn very_long_line() {
        let long = "a".repeat(10000);
        assert_structural_round_trip(&long);
    }

    #[test]
    fn many_paragraphs() {
        let paras: Vec<_> = (0..100).map(|i| format!("Paragraph {}", i)).collect();
        assert_structural_round_trip(&paras.join("\n\n"));
    }

    #[test]
    fn deeply_nested_quotes() {
        let nested = (0..10).map(|i| ">".repeat(i + 1) + " Level").collect::<Vec<_>>().join("\n");
        assert_structural_round_trip(&nested);
    }

    #[test]
    fn special_characters() {
        assert_structural_round_trip("< > & \" ' \\ / $ @ # % ^ * ( ) [ ] { }");
    }

    #[test]
    fn html_entities() {
        assert_structural_round_trip("&amp; &lt; &gt; &quot;");
    }

    #[test]
    fn backslash_escapes() {
        assert_structural_round_trip("\\* \\_ \\` \\[ \\] \\# \\!");
    }

    #[test]
    fn tabs_and_spaces() {
        assert_structural_round_trip("text\twith\ttabs");
    }

    #[test]
    fn crlf_line_endings() {
        assert_structural_round_trip("line one\r\nline two\r\n");
    }

    #[test]
    fn mixed_line_endings() {
        assert_structural_round_trip("unix\nwindows\r\nold mac\r");
    }

    #[test]
    fn trailing_whitespace() {
        assert_structural_round_trip("line with trailing   \nnext line");
    }

    #[test]
    fn multiple_blank_lines() {
        assert_structural_round_trip("paragraph one\n\n\n\n\nparagraph two");
    }

    #[test]
    fn horizontal_rules() {
        assert_structural_round_trip("---\n\n***\n\n___");
    }
}

// =============================================================================
// Combined / Complex Documents
// =============================================================================

mod complex_documents {
    use super::*;

    #[test]
    fn readme_style_document() {
        let doc = r#"# Project Name

A brief description of the project.

## Installation

```bash
cargo install project
```

## Usage

1. First step
2. Second step
3. Third step

## Features

- Feature one
- Feature two
  - Sub-feature
- Feature three

## License

MIT
"#;
        assert_structural_round_trip(doc);
    }

    #[test]
    fn blog_post_style() {
        let doc = r#"---
title: My Blog Post
date: 2024-01-15
tags: [rust, crdt]
---

# Introduction

This is the **first** paragraph with some *emphasis*.

> A wise quote from someone

## Code Example

Here's some code:

```rust
fn main() {
    println!("Hello!");
}
```

## Conclusion

Thanks for reading!
"#;
        assert_structural_round_trip(doc);
    }

    #[test]
    fn documentation_style() {
        let doc = r#"# API Reference

## `function_name(arg1, arg2)`

Does something useful.

### Parameters

| Name | Type | Description |
|------|------|-------------|
| arg1 | `String` | First argument |
| arg2 | `i32` | Second argument |

### Returns

Returns a `Result<(), Error>`.

### Example

```rust
function_name("hello", 42)?;
```
"#;
        assert_structural_round_trip(doc);
    }
}
