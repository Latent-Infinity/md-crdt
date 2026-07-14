use crate::core::{LwwRegister, OpId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frontmatter {
    raw: String,
    fields: BTreeMap<String, LwwRegister<Option<String>>>,
    original: BTreeMap<String, String>,
    dirty: BTreeSet<String>,
    structured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrontmatterError {
    #[error("frontmatter uses unsupported or malformed YAML; structured mutation is disabled")]
    Opaque,
    #[error("frontmatter key must be a non-empty top-level YAML key")]
    InvalidKey,
}

impl Frontmatter {
    pub fn parse(raw: String) -> Self {
        let mut fields = BTreeMap::new();
        let mut original = BTreeMap::new();
        let mut structured = true;
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if line.starts_with(char::is_whitespace) || trimmed.starts_with("- ") {
                structured = false;
                continue;
            }
            let Some((key, rest)) = line.split_once(':') else {
                structured = false;
                continue;
            };
            let key = key.trim();
            if !valid_key(key) || fields.contains_key(key) {
                structured = false;
                continue;
            }
            let value = value_without_comment(rest.trim()).to_string();
            if matches!(value.as_str(), "|" | ">" | "|-" | ">-") {
                structured = false;
            }
            let seed = OpId {
                counter: 0,
                peer: 0,
            };
            fields.insert(key.to_string(), LwwRegister::new(Some(value.clone()), seed));
            original.insert(key.to_string(), value);
        }
        Self {
            raw,
            fields,
            original,
            dirty: BTreeSet::new(),
            structured,
        }
    }

    pub fn empty() -> Self {
        Self::parse(String::new())
    }

    pub fn is_structured(&self) -> bool {
        self.structured
    }

    pub fn is_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key)?.get_ref().as_deref()
    }

    pub fn entries(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.fields
            .iter()
            .map(|(key, value)| (key.as_str(), value.get_ref().as_deref()))
    }

    pub fn set(
        &mut self,
        key: String,
        value: Option<String>,
        op_id: OpId,
    ) -> Result<(), FrontmatterError> {
        if !self.structured {
            return Err(FrontmatterError::Opaque);
        }
        if !valid_key(&key) {
            return Err(FrontmatterError::InvalidKey);
        }
        self.fields
            .entry(key.clone())
            .and_modify(|register| register.set(value.clone(), op_id))
            .or_insert_with(|| LwwRegister::new(value, op_id));
        self.dirty.insert(key);
        Ok(())
    }

    pub fn render(&self) -> String {
        if self.dirty.is_empty() {
            return self.raw.clone();
        }
        let mut output = Vec::new();
        let mut emitted = BTreeSet::new();
        for line in self.raw.lines() {
            let Some((key_part, rest)) = line.split_once(':') else {
                output.push(line.to_string());
                continue;
            };
            let key = key_part.trim();
            if !self.dirty.contains(key) {
                output.push(line.to_string());
                continue;
            }
            emitted.insert(key.to_string());
            if let Some(value) = self
                .fields
                .get(key)
                .and_then(|register| register.get_ref().as_ref())
            {
                let comment = inline_comment(rest).unwrap_or_default();
                let spacing = if rest.starts_with(' ') { " " } else { "" };
                output.push(format!("{key_part}:{spacing}{value}{comment}"));
            }
        }
        for key in &self.dirty {
            if emitted.contains(key) || self.original.contains_key(key) {
                continue;
            }
            if let Some(value) = self
                .fields
                .get(key)
                .and_then(|register| register.get_ref().as_ref())
            {
                output.push(format!("{key}: {value}"));
            }
        }
        output.join("\n")
    }
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

fn inline_comment(value: &str) -> Option<&str> {
    let mut single = false;
    let mut double = false;
    for (index, ch) in value.char_indices() {
        match ch {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '#' if !single && !double && value[..index].ends_with(char::is_whitespace) => {
                return Some(&value[index.saturating_sub(1)..]);
            }
            _ => {}
        }
    }
    None
}

fn value_without_comment(value: &str) -> &str {
    inline_comment(value)
        .map(|comment| &value[..value.len() - comment.len()])
        .unwrap_or(value)
        .trim_end()
}
