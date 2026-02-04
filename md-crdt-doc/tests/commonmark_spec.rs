//! CommonMark spec compliance tests.
//!
//! These tests verify that our parser can handle all CommonMark spec examples
//! without crashing and that round-trip parsing is idempotent.

use md_crdt_doc::{EquivalenceMode, Parser, SerializeConfig};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct SpecExample {
    markdown: String,
    #[allow(dead_code)]
    html: String, // Kept for potential future HTML output comparison
    example: u32,
    section: String,
}

fn load_spec() -> Vec<SpecExample> {
    let spec_json = include_str!("fixtures/commonmark-spec.json");
    serde_json::from_str(spec_json).expect("Failed to parse CommonMark spec")
}

/// Test that all CommonMark examples parse without panicking.
#[test]
fn commonmark_spec_parses_without_panic() {
    let examples = load_spec();
    let mut failures = Vec::new();

    for example in &examples {
        let result = std::panic::catch_unwind(|| {
            let _ = Parser::parse(&example.markdown);
        });

        if result.is_err() {
            failures.push(format!(
                "Example {} ({}): panic on parse",
                example.example, example.section
            ));
        }
    }

    if !failures.is_empty() {
        panic!(
            "Parser panicked on {} examples:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

/// Test that all CommonMark examples round-trip (parse -> serialize -> parse -> serialize)
/// produces stable output (idempotent after first normalization).
#[test]
fn commonmark_spec_round_trip_idempotent() {
    let examples = load_spec();
    let config = SerializeConfig::structural();
    let mut failures = Vec::new();

    for example in &examples {
        let result = std::panic::catch_unwind(|| {
            let doc = Parser::parse(&example.markdown);
            let output1 = doc.serialize_with_config(&config);
            let doc2 = Parser::parse(&output1);
            let output2 = doc2.serialize_with_config(&config);
            (output1, output2)
        });

        match result {
            Ok((output1, output2)) => {
                if output1 != output2 {
                    failures.push(format!(
                        "Example {} ({}): round-trip not idempotent\n  First:  {:?}\n  Second: {:?}",
                        example.example, example.section, output1, output2
                    ));
                }
            }
            Err(_) => {
                failures.push(format!(
                    "Example {} ({}): panic during round-trip",
                    example.example, example.section
                ));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "Round-trip failed for {} examples:\n{}",
            failures.len(),
            failures.join("\n\n")
        );
    }
}

/// Test parsing by section to help identify which markdown features need work.
#[test]
fn commonmark_spec_by_section() {
    let examples = load_spec();
    let mut sections: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();

    for example in &examples {
        let entry = sections.entry(example.section.clone()).or_insert((0, 0));
        let result = std::panic::catch_unwind(|| {
            let _ = Parser::parse(&example.markdown);
        });
        entry.0 += 1; // total
        if result.is_ok() {
            entry.1 += 1; // passed
        }
    }

    let mut section_list: Vec<_> = sections.into_iter().collect();
    section_list.sort_by_key(|(name, _)| name.clone());

    println!("\nCommonMark spec coverage by section:");
    println!(
        "{:<40} {:>6} {:>6} {:>6}",
        "Section", "Total", "Pass", "Rate"
    );
    println!("{}", "-".repeat(60));

    for (section, (total, passed)) in &section_list {
        let rate = (*passed as f64 / *total as f64) * 100.0;
        println!("{:<40} {:>6} {:>6} {:>5.1}%", section, total, passed, rate);
    }
}

// Individual section tests for granular failure tracking
mod sections {
    use super::*;

    macro_rules! section_test {
        ($name:ident, $section:literal) => {
            #[test]
            fn $name() {
                let examples = load_spec();
                let section_examples: Vec<_> =
                    examples.iter().filter(|e| e.section == $section).collect();

                for example in section_examples {
                    let doc = Parser::parse(&example.markdown);
                    let _ = doc.serialize(EquivalenceMode::Structural);
                }
            }
        };
    }

    section_test!(paragraphs, "Paragraphs");
    section_test!(block_quotes, "Block quotes");
    section_test!(code_spans, "Code spans");
    section_test!(fenced_code_blocks, "Fenced code blocks");
    section_test!(indented_code_blocks, "Indented code blocks");
    section_test!(atx_headings, "ATX headings");
    section_test!(setext_headings, "Setext headings");
    section_test!(thematic_breaks, "Thematic breaks");
    section_test!(links, "Links");
    section_test!(images, "Images");
    section_test!(emphasis, "Emphasis and strong emphasis");
    section_test!(lists, "Lists");
    section_test!(list_items, "List items");
    section_test!(hard_line_breaks, "Hard line breaks");
    section_test!(soft_line_breaks, "Soft line breaks");
    section_test!(textual_content, "Textual content");
}
