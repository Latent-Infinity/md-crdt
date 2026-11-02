use crate::core::{LwwRegister, OpId, StateVector};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frontmatter {
    #[serde(deserialize_with = "Option::deserialize")]
    base: Option<LwwRegister<FrontmatterBase>>,
    // Explicit writes are independent of the initializer's timestamp and raw YAML.
    fields: BTreeMap<String, LwwRegister<Option<String>>>,
    initializers: StateVector,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FrontmatterBase {
    raw: String,
    original: BTreeMap<String, String>,
    structured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FrontmatterRevision {
    #[serde(deserialize_with = "Option::deserialize")]
    base: Option<OpId>,
    fields: BTreeMap<String, OpId>,
    pub(crate) source_structured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrontmatterError {
    #[error("frontmatter uses unsupported or malformed YAML; structured mutation is disabled")]
    Opaque,
    #[error("frontmatter key must be a non-empty top-level YAML key")]
    InvalidKey,
}

impl FrontmatterBase {
    fn parse(raw: String) -> Self {
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
            if !valid_key(key) || original.contains_key(key) {
                structured = false;
                continue;
            }
            let value = value_without_comment(rest.trim()).to_string();
            if matches!(value.as_str(), "|" | ">" | "|-" | ">-") {
                structured = false;
            }
            original.insert(key.to_string(), value);
        }
        Self {
            raw,
            original,
            structured,
        }
    }
}

impl Frontmatter {
    pub fn parse(raw: String) -> Self {
        Self {
            base: Some(LwwRegister::new(
                FrontmatterBase::parse(raw),
                OpId {
                    counter: 0,
                    peer: 0,
                },
            )),
            fields: BTreeMap::new(),
            initializers: StateVector::new(),
        }
    }

    pub fn empty() -> Self {
        Self::parse(String::new())
    }

    pub(crate) fn without_base() -> Self {
        Self {
            base: None,
            fields: BTreeMap::new(),
            initializers: StateVector::new(),
        }
    }

    pub fn has_base(&self) -> bool {
        self.base.is_some()
    }

    pub(crate) fn initialize(&mut self, frontmatter: &Self, op_id: OpId) {
        self.initializers.set(
            op_id.peer,
            self.initializers
                .get(op_id.peer)
                .unwrap_or(0)
                .max(op_id.counter),
        );
        if self.base.as_ref().is_some_and(|base| base.op_id() > op_id) {
            return;
        }
        // An initializer contributes only its rendered base, never embedded write history.
        let base = FrontmatterBase::parse(frontmatter.render());
        if let Some(register) = &mut self.base {
            register.set(base, op_id);
        } else {
            self.base = Some(LwwRegister::new(base, op_id));
        }
    }

    pub fn is_structured(&self) -> bool {
        self.base
            .as_ref()
            .is_none_or(|base| base.get_ref().structured)
    }

    pub fn is_dirty(&self) -> bool {
        (self.is_structured() && !self.fields.is_empty())
            || self
                .base
                .as_ref()
                .is_some_and(|base| base.op_id().counter != 0)
    }

    pub(crate) fn source_revision(&self, source_structured: bool) -> FrontmatterRevision {
        FrontmatterRevision {
            base: self.base.as_ref().map(LwwRegister::op_id),
            source_structured,
            fields: self
                .fields
                .iter()
                .filter(|_| self.is_structured())
                .map(|(key, register)| (key.clone(), register.op_id()))
                .collect(),
        }
    }

    pub(crate) fn matches_source_revision(&self, revision: &FrontmatterRevision) -> bool {
        self.matches_source_base(revision)
            && self
                .fields
                .iter()
                .filter(|_| self.is_structured())
                .map(|(key, register)| (key, register.op_id()))
                .eq(revision.fields.iter().map(|(key, id)| (key, *id)))
    }

    pub(crate) fn matches_source_base(&self, revision: &FrontmatterRevision) -> bool {
        self.base.as_ref().map(LwwRegister::op_id) == revision.base
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        if self.is_structured()
            && let Some(value) = self.fields.get(key)
        {
            return value.get_ref().as_deref();
        }
        self.base
            .as_ref()?
            .get_ref()
            .original
            .get(key)
            .map(String::as_str)
    }

    pub fn entries(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        let structured = self.is_structured();
        self.base
            .iter()
            .flat_map(|base| &base.get_ref().original)
            .filter(move |(key, _)| !structured || !self.fields.contains_key(*key))
            .map(|(key, value)| (key.as_str(), Some(value.as_str())))
            .chain(
                self.fields
                    .iter()
                    .filter(move |_| structured)
                    .map(|(key, value)| (key.as_str(), value.get_ref().as_deref())),
            )
    }

    pub(crate) fn max_counter_for_peer(&self, peer: crate::core::PeerId) -> u64 {
        self.fields
            .values()
            .map(LwwRegister::op_id)
            .chain(self.base.iter().map(LwwRegister::op_id))
            .filter(|id| id.peer == peer)
            .map(|id| id.counter)
            .max()
            .unwrap_or(0)
            .max(self.initializers.get(peer).unwrap_or(0))
    }

    pub fn set(
        &mut self,
        key: String,
        value: Option<String>,
        op_id: OpId,
    ) -> Result<(), FrontmatterError> {
        if !self.is_structured() {
            return Err(FrontmatterError::Opaque);
        }
        self.receive_field(key, value, op_id)
    }

    pub(crate) fn receive_field(
        &mut self,
        key: String,
        value: Option<String>,
        op_id: OpId,
    ) -> Result<(), FrontmatterError> {
        if !valid_key(&key) {
            return Err(FrontmatterError::InvalidKey);
        }
        // A concurrent opaque base hides, but must not discard, supported writes.
        self.fields
            .entry(key)
            .and_modify(|register| register.set(value.clone(), op_id))
            .or_insert_with(|| LwwRegister::new(value, op_id));
        Ok(())
    }

    pub fn render(&self) -> String {
        let base = self.base.as_ref().map(LwwRegister::get_ref);
        let raw = base.map_or("", |base| base.raw.as_str());
        self.render_on(raw)
    }

    pub(crate) fn render_on(&self, raw: &str) -> String {
        if self.fields.is_empty() || !self.is_structured() {
            return raw.to_string();
        }
        let mut output = Vec::new();
        let mut emitted = BTreeSet::new();
        for line in raw.lines() {
            let Some((key_part, rest)) = line.split_once(':') else {
                output.push(line.to_string());
                continue;
            };
            let key = key_part.trim();
            let Some(register) = self.fields.get(key) else {
                output.push(line.to_string());
                continue;
            };
            emitted.insert(key.to_string());
            if let Some(value) = register.get_ref() {
                let comment = inline_comment(rest).unwrap_or_default();
                let spacing = if rest.starts_with(' ') { " " } else { "" };
                output.push(format!("{key_part}:{spacing}{value}{comment}"));
            }
        }
        for (key, register) in &self.fields {
            if emitted.contains(key) {
                continue;
            }
            if let Some(value) = register.get_ref() {
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
                let (start, _) = value[..index].char_indices().next_back()?;
                return Some(&value[start..]);
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
