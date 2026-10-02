//! Resource bounds applied before file ingestion or untrusted Markdown parsing.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// Maximum Markdown input size (4 MiB), before CRDT expansion.
pub const MAX_MARKDOWN_BYTES: usize = 4 * 1024 * 1024;
/// Maximum leading block-quote markers on a source line.
pub const MAX_QUOTE_DEPTH: usize = 64;
/// Maximum leading whitespace bytes on a nonempty source line.
pub const MAX_INDENT_BYTES: usize = 256;
/// Maximum inline delimiter bytes between blank source lines, bounding mark work.
pub const MAX_INLINE_DELIMITERS: usize = 16_384;

/// A source input exceeds a supported resource budget.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InputLimitError {
    /// Source byte budget exceeded.
    #[error("Markdown exceeds the {MAX_MARKDOWN_BYTES}-byte input limit")]
    Bytes,
    /// Quadratic text alignment would exceed the work budget.
    #[error("text alignment exceeds the 4,000,000-cell work limit")]
    DiffWork,
    /// Inline mark parsing would exceed the source work budget.
    #[error("Markdown exceeds the inline syntax work budget")]
    InlineWork,
    /// Source nesting budget exceeded.
    #[error("Markdown exceeds the nesting budget at line {line}")]
    Nesting {
        /// One-based source line.
        line: usize,
    },
}

/// Validate source size and conservative lexical nesting budgets before parsing.
///
/// These limits also apply inside fences: validation is deliberately a bounded
/// lexical pass, independent of semantic parsing. No source text is logged.
pub fn validate_markdown(markdown: &str) -> Result<(), InputLimitError> {
    if markdown.len() > MAX_MARKDOWN_BYTES {
        return Err(InputLimitError::Bytes);
    }
    let mut delimiters = 0usize;
    let mut brackets = 0usize;
    let mut paragraph_bytes = 0usize;
    let mut completed_work = 0usize;
    for (index, line) in markdown.lines().enumerate() {
        if line.trim().is_empty() {
            completed_work =
                completed_work.saturating_add(paragraph_bytes.saturating_mul(delimiters));
            paragraph_bytes = 0;
            delimiters = 0;
            brackets = 0;
        } else {
            paragraph_bytes = paragraph_bytes.saturating_add(line.len());
            for byte in line.bytes() {
                if matches!(byte, b'*' | b'_' | b'~' | b'[' | b']' | b'`') {
                    delimiters += 1;
                }
                if byte == b'[' {
                    brackets += 1;
                }
                if byte == b']' {
                    brackets = brackets.saturating_sub(1);
                }
                if brackets > 64 {
                    return Err(InputLimitError::InlineWork);
                }
            }
        }
        // Inline parsing scans delimiter candidates and visible prefixes. Budget
        // their conservative product, rather than rejecting many small blocks.
        if delimiters > MAX_INLINE_DELIMITERS
            || completed_work.saturating_add(paragraph_bytes.saturating_mul(delimiters))
                > 64 * 1024 * 1024
        {
            return Err(InputLimitError::InlineWork);
        }
        let mut rest = line;
        let mut quotes = 0;
        let mut indent = 0;
        loop {
            let trimmed = rest.trim_start();
            indent += rest.len() - trimmed.len();
            rest = trimmed;
            if let Some(tail) = rest.strip_prefix('>') {
                quotes += 1;
                rest = tail;
            } else {
                break;
            }
        }
        if quotes > MAX_QUOTE_DEPTH || (!rest.is_empty() && indent > MAX_INDENT_BYTES) {
            return Err(InputLimitError::Nesting { line: index + 1 });
        }
    }
    Ok(())
}

/// Read at most the Markdown budget plus one byte, then validate before parsing.
///
/// The bounded reader also covers files growing after metadata inspection.
pub fn read_markdown_file(path: &Path) -> io::Result<String> {
    let text = String::from_utf8(read_file_bounded(path, MAX_MARKDOWN_BYTES)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    validate_markdown(&text).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(text)
}

/// Read a bounded binary artifact without trusting its declared size.
pub fn read_file_bounded(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > max_bytes as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds input byte limit",
        ));
    }
    let mut bytes = Vec::new();
    file.take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds input byte limit",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_budgets_accept_boundaries_and_reject_excess() {
        assert_eq!(
            validate_markdown(&"[".repeat(65)),
            Err(InputLimitError::InlineWork)
        );
        assert_eq!(
            validate_markdown(&"*".repeat(MAX_INLINE_DELIMITERS + 1)),
            Err(InputLimitError::InlineWork)
        );
        assert!(validate_markdown(&"x".repeat(MAX_MARKDOWN_BYTES)).is_ok());
        assert_eq!(
            validate_markdown(&"x".repeat(MAX_MARKDOWN_BYTES + 1)),
            Err(InputLimitError::Bytes)
        );
        assert!(validate_markdown(&format!("{}text", "> ".repeat(MAX_QUOTE_DEPTH))).is_ok());
        assert_eq!(
            validate_markdown(&format!("{}text", "> ".repeat(MAX_QUOTE_DEPTH + 1))),
            Err(InputLimitError::Nesting { line: 1 })
        );
        assert!(validate_markdown(&format!("{}text", " ".repeat(MAX_INDENT_BYTES + 1))).is_err());
    }

    #[test]
    fn many_small_marked_paragraphs_fit_but_one_expensive_paragraph_does_not() {
        assert!(validate_markdown(&"item **bold** [link](#target)\n\n".repeat(10_000)).is_ok());
        assert_eq!(
            validate_markdown(&format!(
                "{}{}",
                "x".repeat(100_000),
                "**bold**".repeat(200)
            )),
            Err(InputLimitError::InlineWork)
        );
    }

    #[test]
    fn oversized_file_is_rejected_without_materializing_it() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file()
            .set_len(MAX_MARKDOWN_BYTES as u64 + 1)
            .unwrap();
        assert_eq!(
            read_markdown_file(file.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
