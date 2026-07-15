use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(transparent)]
pub struct FactKey(String);

impl FactKey {
    pub fn new(value: impl Into<String>) -> Result<Self, FactError> {
        let value = value.into();
        if valid_fact_key(&value) {
            Ok(Self(value))
        } else {
            Err(FactError::new("fact key is invalid"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FactKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for FactKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FactRef {
    Artifact { key: FactKey },
    Board { key: FactKey },
}

impl FactRef {
    pub fn artifact(key: impl Into<String>) -> Result<Self, FactError> {
        Ok(Self::Artifact {
            key: FactKey::new(key)?,
        })
    }

    pub fn board(key: impl Into<String>) -> Result<Self, FactError> {
        Ok(Self::Board {
            key: FactKey::new(key)?,
        })
    }

    pub fn key(&self) -> &FactKey {
        match self {
            Self::Artifact { key } | Self::Board { key } => key,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Artifact { .. } => "artifact",
            Self::Board { .. } => "board",
        }
    }
}

impl fmt::Display for FactRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.kind(), self.key())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct ArtifactRef {
    key: FactKey,
}

impl ArtifactRef {
    pub fn new(key: impl Into<String>) -> Result<Self, FactError> {
        Ok(Self {
            key: FactKey::new(key)?,
        })
    }

    pub fn key(&self) -> &FactKey {
        &self.key
    }

    pub fn fact_ref(&self) -> FactRef {
        FactRef::Artifact {
            key: self.key.clone(),
        }
    }
}

impl fmt::Display for ArtifactRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "artifact.{}", self.key)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FlowPredicate {
    Exists { fact: FactRef },
    Truthy { fact: FactRef },
}

impl FlowPredicate {
    pub fn exists(fact: FactRef) -> Self {
        Self::Exists { fact }
    }

    pub fn truthy(fact: FactRef) -> Self {
        Self::Truthy { fact }
    }

    pub fn exists_artifact(key: impl Into<String>) -> Result<Self, FactError> {
        Ok(Self::exists(FactRef::artifact(key)?))
    }

    pub fn truthy_artifact(key: impl Into<String>) -> Result<Self, FactError> {
        Ok(Self::truthy(FactRef::artifact(key)?))
    }

    pub fn exists_board(key: impl Into<String>) -> Result<Self, FactError> {
        Ok(Self::exists(FactRef::board(key)?))
    }

    pub fn truthy_board(key: impl Into<String>) -> Result<Self, FactError> {
        Ok(Self::truthy(FactRef::board(key)?))
    }

    pub fn fact_ref(&self) -> &FactRef {
        match self {
            Self::Exists { fact } | Self::Truthy { fact } => fact,
        }
    }

    pub fn matches(&self, value: Option<&str>) -> bool {
        match self {
            Self::Exists { .. } => value.is_some(),
            Self::Truthy { .. } => value.is_some_and(truthy_value),
        }
    }
}

impl fmt::Display for FlowPredicate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exists { fact } => write!(formatter, "exists({fact})"),
            Self::Truthy { fact } => write!(formatter, "truthy({fact})"),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FactError {
    message: String,
}

impl FactError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for FactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FactError {}

fn valid_fact_key(key: &str) -> bool {
    !key.is_empty()
        && key.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        })
}

fn truthy_value(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value != "false" && value != "0"
}
