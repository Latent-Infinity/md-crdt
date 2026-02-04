//! Tests using external markdown fixtures from markdown-it, Comrak, and GFM spec.
//!
//! These tests verify parser robustness against real-world test cases from other
//! markdown implementations.

use md_crdt_doc::{Parser, SerializeConfig};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ExternalExample {
    markdown: String,
    #[allow(dead_code)]
    html: String,
    example: u32,
    section: String,
    source: String,
}

fn load_fixtures(name: &str) -> Vec<ExternalExample> {
    let path = format!("tests/fixtures/{}", name);
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("Failed to read fixture file: {}", path));
    serde_json::from_str(&content).unwrap_or_else(|e| panic!("Failed to parse {}: {}", path, e))
}

/// Test that all markdown-it examples parse without panicking.
#[test]
fn markdown_it_parses_without_panic() {
    let examples = load_fixtures("markdown-it-fixtures.json");
    let mut failures = Vec::new();

    for example in &examples {
        let result = std::panic::catch_unwind(|| {
            let _ = Parser::parse(&example.markdown);
        });

        if result.is_err() {
            failures.push(format!(
                "[{}] Example {} ({}): panic on parse",
                example.source, example.example, example.section
            ));
        }
    }

    if !failures.is_empty() {
        panic!(
            "Parser panicked on {} markdown-it examples:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

/// Test that all Comrak examples parse without panicking.
#[test]
fn comrak_parses_without_panic() {
    let examples = load_fixtures("comrak-fixtures.json");
    let mut failures = Vec::new();

    for example in &examples {
        let result = std::panic::catch_unwind(|| {
            let _ = Parser::parse(&example.markdown);
        });

        if result.is_err() {
            failures.push(format!(
                "[{}] Example {} ({}): panic on parse",
                example.source, example.example, example.section
            ));
        }
    }

    if !failures.is_empty() {
        panic!(
            "Parser panicked on {} Comrak examples:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

/// Test that all GFM spec examples parse without panicking.
#[test]
fn gfm_spec_parses_without_panic() {
    let examples = load_fixtures("gfm-spec-fixtures.json");
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
            "Parser panicked on {} GFM spec examples:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

/// Test round-trip stability for all external fixtures.
#[test]
fn external_fixtures_round_trip_stable() {
    let examples = load_fixtures("all-external-fixtures.json");
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
                        "[{}] Example {} ({}): round-trip not idempotent",
                        example.source, example.example, example.section
                    ));
                }
            }
            Err(_) => {
                failures.push(format!(
                    "[{}] Example {} ({}): panic during round-trip",
                    example.source, example.example, example.section
                ));
            }
        }
    }

    if !failures.is_empty() {
        // Print first 20 failures to avoid overwhelming output
        let shown = failures.iter().take(20).cloned().collect::<Vec<_>>();
        panic!(
            "Round-trip failed for {} examples (showing first 20):\n{}",
            failures.len(),
            shown.join("\n")
        );
    }
}

/// Coverage report by source.
#[test]
fn external_fixtures_coverage_report() {
    let examples = load_fixtures("all-external-fixtures.json");
    let mut by_source: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();

    for example in &examples {
        let entry = by_source.entry(example.source.clone()).or_insert((0, 0));
        let result = std::panic::catch_unwind(|| {
            let _ = Parser::parse(&example.markdown);
        });
        entry.0 += 1; // total
        if result.is_ok() {
            entry.1 += 1; // passed
        }
    }

    println!("\nExternal fixtures coverage by source:");
    println!(
        "{:<50} {:>6} {:>6} {:>6}",
        "Source", "Total", "Pass", "Rate"
    );
    println!("{}", "-".repeat(70));

    let mut sources: Vec<_> = by_source.into_iter().collect();
    sources.sort_by_key(|(name, _)| name.clone());

    for (source, (total, passed)) in &sources {
        let rate = (*passed as f64 / *total as f64) * 100.0;
        println!("{:<50} {:>6} {:>6} {:>5.1}%", source, total, passed, rate);
    }
}
