//! One immutable operation request with an ordered chain of context wrappers.
//! Wrappers are stored inner-to-outer, exactly as printed by the pipeline syntax.
mod text;

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::BTreeMap;

pub const VERSION: u16 = 3;
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_CONTEXTS: usize = 32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Value {
    Bytes(Vec<u8>),
    U64(u64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Success { value: Value },
    Error { failure: Failure },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Failure {
    Failed {
        domain: String,
        code: String,
        effects: Json,
    },
    Denied {
        reason: String,
    },
    Unsupported {
        reason: String,
    },
    Unknown {
        reason: String,
        known_effects: Json,
    },
}
impl Outcome {
    pub fn success(value: Value) -> Self {
        Self::Success { value }
    }
    pub fn denied(reason: impl Into<String>) -> Self {
        Self::Error {
            failure: Failure::Denied {
                reason: reason.into(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpCode {
    #[serde(rename = "fs.read")]
    Read,
    #[serde(rename = "fs.write")]
    Write,
}
impl OpCode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "fs.read",
            Self::Write => "fs.write",
        }
    }
}

/// The request's logical resource and arguments; contexts do not mutate this term.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
pub enum Operation {
    #[serde(rename = "fs.read")]
    Read {
        file: String,
        offset: u64,
        length: u64,
    },
    #[serde(rename = "fs.write")]
    Write {
        file: String,
        offset: u64,
        data: Vec<u8>,
    },
}
impl Operation {
    pub fn code(&self) -> OpCode {
        match self {
            Self::Read { .. } => OpCode::Read,
            Self::Write { .. } => OpCode::Write,
        }
    }
    pub fn file(&self) -> &str {
        match self {
            Self::Read { file, .. } | Self::Write { file, .. } => file,
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.file().is_empty(), "empty file reference");
        let (offset, length) = match self {
            Self::Read { offset, length, .. } => (*offset, *length),
            Self::Write { offset, data, .. } => (*offset, data.len() as u64),
        };
        ensure!(offset.checked_add(length).is_some(), "file range overflow");
        Ok(())
    }
    pub fn check_outcome(&self, outcome: &Outcome) -> Result<()> {
        if let Outcome::Success { value } = outcome {
            match (self, value) {
                (Self::Read { length, .. }, Value::Bytes(bytes)) => ensure!(
                    bytes.len() as u64 <= *length,
                    "read exceeds requested length"
                ),
                (Self::Write { data, .. }, Value::U64(count)) => {
                    ensure!(*count <= data.len() as u64, "write exceeds supplied bytes")
                }
                _ => bail!("operation result type mismatch"),
            }
        }
        Ok(())
    }
}

/// Placement wrappers preserve the operation's result contract. Mock/deny are
/// terminal handlers and must be outermost; their inner computation is not run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "context", rename_all = "snake_case", deny_unknown_fields)]
pub enum Layer {
    Vm { name: String },
    Remote { name: String },
    Overlay { name: String },
    Mock { value: Value },
    Deny { reason: String },
}
impl Layer {
    pub fn terminal(&self) -> Option<Outcome> {
        match self {
            Self::Mock { value } => Some(Outcome::success(value.clone())),
            Self::Deny { reason } => Some(Outcome::denied(reason.clone())),
            _ => None,
        }
    }
}
fn validate_layers(layers: &[Layer]) -> Result<()> {
    ensure!(layers.len() <= MAX_CONTEXTS, "too many contexts");
    for (index, layer) in layers.iter().enumerate() {
        match layer {
            Layer::Vm { name } | Layer::Remote { name } | Layer::Overlay { name } => {
                ensure!(!name.trim().is_empty(), "empty context name")
            }
            Layer::Deny { reason } => ensure!(!reason.trim().is_empty(), "empty denial reason"),
            Layer::Mock { .. } => {}
        }
        ensure!(
            !matches!(layer, Layer::Mock { .. } | Layer::Deny { .. }) || index + 1 == layers.len(),
            "mock/deny must be the outermost context"
        );
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expression {
    pub version: u16,
    pub operation: Operation,
    pub contexts: Vec<Layer>,
}
impl Expression {
    pub fn new(operation: Operation) -> Self {
        Self {
            version: VERSION,
            operation,
            contexts: Vec::new(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == VERSION,
            "unsupported IR version {}",
            self.version
        );
        self.operation.validate()?;
        validate_layers(&self.contexts)?;
        if let Some(outcome) = self.contexts.last().and_then(Layer::terminal) {
            self.operation.check_outcome(&outcome)?;
        }
        ensure!(
            serde_json::to_vec(self)?.len() <= MAX_TEXT_BYTES,
            "expression exceeds size limit"
        );
        Ok(())
    }
    pub fn to_text(&self) -> Result<String> {
        self.validate()?;
        Ok(self.to_string())
    }
}

/// Trusted execution metadata is outside the operation expression. A binding is
/// a description of a capability; the backend must check actual authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub backend: String,
    pub resource: String,
    pub generation: u64,
    pub contract: u16,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub revision: u64,
    pub principal: String,
    pub scope: Vec<String>,
    pub policy: String,
    #[serde(deserialize_with = "unique_map")]
    pub bindings: BTreeMap<String, Binding>,
}
impl Context {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.principal.trim().is_empty() && !self.policy.trim().is_empty(),
            "context requires principal and policy"
        );
        ensure!(
            !self.scope.is_empty()
                && self.scope.len() <= 16
                && self
                    .scope
                    .iter()
                    .all(|s| !s.trim().is_empty() && s.len() <= 256),
            "invalid scope"
        );
        for (name, binding) in &self.bindings {
            ensure!(
                !name.is_empty()
                    && !binding.backend.is_empty()
                    && !binding.resource.is_empty()
                    && binding.contract == 1,
                "invalid resource binding"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pattern {
    pub operation: OpCode,
    pub file: Option<String>,
    pub contexts: Option<Vec<Layer>>,
}
impl Pattern {
    pub fn matches(&self, expression: &Expression) -> bool {
        self.operation == expression.operation.code()
            && self
                .file
                .as_deref()
                .is_none_or(|file| file == expression.operation.file())
            && self
                .contexts
                .as_ref()
                .is_none_or(|layers| layers == &expression.contexts)
    }
}

/// The normal rewrite edits only the suffix. Replace is explicit and its
/// derived operation never overwrites the original request held by the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "rewrite", rename_all = "snake_case", deny_unknown_fields)]
pub enum Rewrite {
    Append { contexts: Vec<Layer> },
    SetContexts { contexts: Vec<Layer> },
    Replace { expression: Expression },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub version: u16,
    pub pattern: Pattern,
    pub rewrite: Rewrite,
}
impl Rule {
    pub fn validate(&self) -> Result<()> {
        symbol(&self.id)?;
        ensure!(self.version > 0, "rule version must be positive");
        if let Some(file) = &self.pattern.file {
            ensure!(!file.is_empty(), "empty file pattern");
        }
        if let Some(contexts) = &self.pattern.contexts {
            validate_layers(contexts)?;
        }
        match &self.rewrite {
            Rewrite::Append { contexts } | Rewrite::SetContexts { contexts } => {
                validate_layers(contexts)?
            }
            Rewrite::Replace { expression } => {
                expression.validate()?;
                ensure!(
                    expression.operation.code() == self.pattern.operation,
                    "replacement changes operation contract"
                );
            }
        }
        ensure!(
            serde_json::to_vec(self)?.len() <= MAX_TEXT_BYTES,
            "rule exceeds size limit"
        );
        Ok(())
    }
    pub fn apply(&self, before: &Expression) -> Result<Expression> {
        self.validate()?;
        before.validate()?;
        ensure!(
            self.pattern.matches(before),
            "rule does not match expression"
        );
        let mut after = before.clone();
        match &self.rewrite {
            Rewrite::Append { contexts } => after.contexts.extend(contexts.iter().cloned()),
            Rewrite::SetContexts { contexts } => after.contexts = contexts.clone(),
            Rewrite::Replace { expression } => after = expression.clone(),
        }
        after.validate()?;
        Ok(after)
    }
}

pub fn symbol(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
        "invalid symbol {name:?}"
    );
    Ok(())
}

fn unique_map<'de, D, T>(deserializer: D) -> std::result::Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Visitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an object with unique keys")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> std::result::Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, T>()? {
                if result.insert(key.clone(), value).is_some() {
                    return Err(serde::de::Error::custom(format!("duplicate key {key:?}")));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Visitor(std::marker::PhantomData))
}
