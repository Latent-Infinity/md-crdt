//! A minimal parser for Markdown documents.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub frontmatter: Option<String>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    CodeFence(Option<String>),
}

pub struct Parser;

impl Parser {
    pub fn parse(text: &str) -> Document {
        let mut lines = text.lines();
        let mut frontmatter = None;

        let mut remaining = Vec::new();
        if let Some(first) = lines.next() {
            if first.trim() == "---" {
                let mut fm_lines = Vec::new();
                for line in lines.by_ref() {
                    if line.trim() == "---" {
                        frontmatter = Some(fm_lines.join("\n"));
                        break;
                    }
                    fm_lines.push(line);
                }
                remaining.extend(lines.map(|l| l.to_string()));
            } else {
                remaining.push(first.to_string());
                remaining.extend(lines.map(|l| l.to_string()));
            }
        }

        let mut blocks = Vec::new();
        let mut current = Vec::new();
        let mut in_code = false;
        let mut code_info: Option<String> = None;

        for line in remaining {
            let trimmed = line.trim();
            if trimmed.starts_with("```") {
                if !in_code {
                    if !current.is_empty() {
                        blocks.push(Block {
                            kind: BlockKind::Paragraph,
                            text: current.join("\n"),
                        });
                        current.clear();
                    }
                    let info = trimmed.trim_start_matches("```").trim();
                    code_info = if info.is_empty() {
                        None
                    } else {
                        Some(info.to_string())
                    };
                    in_code = true;
                    current.push(line);
                } else {
                    current.push(line);
                    blocks.push(Block {
                        kind: BlockKind::CodeFence(code_info.take()),
                        text: current.join("\n"),
                    });
                    current.clear();
                    in_code = false;
                }
                continue;
            }

            if !in_code && trimmed.is_empty() {
                if !current.is_empty() {
                    blocks.push(Block {
                        kind: BlockKind::Paragraph,
                        text: current.join("\n"),
                    });
                    current.clear();
                }
                continue;
            }

            current.push(line);
        }

        if !current.is_empty() {
            blocks.push(Block {
                kind: if in_code {
                    BlockKind::CodeFence(code_info)
                } else {
                    BlockKind::Paragraph
                },
                text: current.join("\n"),
            });
        }

        Document {
            frontmatter,
            blocks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter_and_blocks() {
        let input = "---\ntitle: Test\n---\n\nHello world\n\nSecond block";
        let doc = Parser::parse(input);
        assert_eq!(doc.frontmatter, Some("title: Test".to_string()));
        assert_eq!(doc.blocks.len(), 2);
        assert_eq!(
            doc.blocks[0],
            Block {
                kind: BlockKind::Paragraph,
                text: "Hello world".to_string(),
            }
        );
        assert_eq!(
            doc.blocks[1],
            Block {
                kind: BlockKind::Paragraph,
                text: "Second block".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_code_fence_block() {
        let input = "Intro\n\n```rust\nfn main() {}\n```\n\nAfter";
        let doc = Parser::parse(input);
        assert_eq!(doc.blocks.len(), 3);
        assert_eq!(
            doc.blocks[1].kind,
            BlockKind::CodeFence(Some("rust".to_string()))
        );
    }
}
