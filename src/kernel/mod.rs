use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const KERNEL_PRIMITIVES: [&str; 6] =
    ["Node", "Contract", "Artifact", "Board", "Route", "Event"];

pub fn kernel_primitive_names() -> [&'static str; 6] {
    KERNEL_PRIMITIVES
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct Node {
    id: String,
    name: String,
    contract_id: String,
}

impl Node {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        contract_id: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            contract_id: contract_id.into(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn contract_id(&self) -> &str {
        &self.contract_id
    }
}

pub type ArtifactPayload = BTreeMap<String, String>;

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct Artifact {
    id: String,
    name: String,
    schema: String,
    payload: ArtifactPayload,
    fingerprint: String,
}

impl Artifact {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        schema: impl Into<String>,
        payload: ArtifactPayload,
    ) -> Self {
        let id = id.into();
        let name = name.into();
        let schema = schema.into();
        let fingerprint = artifact_fingerprint(&id, &name, &schema, &payload);

        Self {
            id,
            name,
            schema,
            payload,
            fingerprint,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn schema(&self) -> &str {
        &self.schema
    }

    pub fn payload(&self) -> &ArtifactPayload {
        &self.payload
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum BoardValue {
    Text(String),
    Bool(bool),
    Number(i64),
    Artifact(String),
}

impl Default for BoardValue {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum BoardChange {
    Set { key: String, value: BoardValue },
    Remove { key: String },
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct BoardPatch {
    expected_version: u64,
    changes: Vec<BoardChange>,
}

impl BoardPatch {
    pub fn new(expected_version: u64) -> Self {
        Self {
            expected_version,
            changes: Vec::new(),
        }
    }

    pub fn set(mut self, key: impl Into<String>, value: BoardValue) -> Self {
        self.changes.push(BoardChange::Set {
            key: key.into(),
            value,
        });
        self
    }

    pub fn remove(mut self, key: impl Into<String>) -> Self {
        self.changes.push(BoardChange::Remove { key: key.into() });
        self
    }

    pub fn expected_version(&self) -> u64 {
        self.expected_version
    }

    pub fn changes(&self) -> &[BoardChange] {
        &self.changes
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum BoardPatchError {
    VersionConflict { expected: u64, actual: u64 },
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct Board {
    id: String,
    version: u64,
    values: BTreeMap<String, BoardValue>,
}

impl Board {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            version: 0,
            values: BTreeMap::new(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn get(&self, key: &str) -> Option<&BoardValue> {
        self.values.get(key)
    }

    pub fn values(&self) -> &BTreeMap<String, BoardValue> {
        &self.values
    }

    pub fn apply(&mut self, patch: BoardPatch) -> Result<u64, BoardPatchError> {
        if patch.expected_version != self.version {
            return Err(BoardPatchError::VersionConflict {
                expected: patch.expected_version,
                actual: self.version,
            });
        }

        for change in patch.changes {
            match change {
                BoardChange::Set { key, value } => {
                    self.values.insert(key, value);
                }
                BoardChange::Remove { key } => {
                    self.values.remove(&key);
                }
            }
        }

        self.version += 1;
        Ok(self.version)
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct ContractRequirement {
    artifact_schema: String,
}

impl ContractRequirement {
    pub fn artifact_schema(schema: impl Into<String>) -> Self {
        Self {
            artifact_schema: schema.into(),
        }
    }

    pub fn schema(&self) -> &str {
        &self.artifact_schema
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct ContractProduction {
    artifact_schema: String,
}

impl ContractProduction {
    pub fn artifact_schema(schema: impl Into<String>) -> Self {
        Self {
            artifact_schema: schema.into(),
        }
    }

    pub fn schema(&self) -> &str {
        &self.artifact_schema
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum ContractPermit {
    #[default]
    RecordEffect,
    ApplyFlow,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "predicate", rename_all = "snake_case")]
pub enum CompletionRule {
    #[default]
    Manual,
    AllProducedArtifactsRecorded,
    Predicate(crate::flow::FlowPredicate),
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct Contract {
    id: String,
    requires: Vec<ContractRequirement>,
    produces: Vec<ContractProduction>,
    permits: Vec<ContractPermit>,
    completion: CompletionRule,
}

impl Contract {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            requires: Vec::new(),
            produces: Vec::new(),
            permits: Vec::new(),
            completion: CompletionRule::Manual,
        }
    }

    pub fn require(mut self, requirement: ContractRequirement) -> Self {
        self.requires.push(requirement);
        self
    }

    pub fn produce(mut self, production: ContractProduction) -> Self {
        self.produces.push(production);
        self
    }

    pub fn permit(mut self, permit: ContractPermit) -> Self {
        self.permits.push(permit);
        self
    }

    pub fn with_completion(mut self, completion: CompletionRule) -> Self {
        self.completion = completion;
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn required_artifacts(&self) -> &[ContractRequirement] {
        &self.requires
    }

    pub fn produced_artifacts(&self) -> &[ContractProduction] {
        &self.produces
    }

    pub fn requires(&self) -> &[ContractRequirement] {
        &self.requires
    }

    pub fn produces(&self) -> &[ContractProduction] {
        &self.produces
    }

    pub fn permits_list(&self) -> &[ContractPermit] {
        &self.permits
    }

    pub fn permits(&self) -> &[ContractPermit] {
        &self.permits
    }

    pub fn completion_rule(&self) -> &CompletionRule {
        &self.completion
    }

    pub fn completion(&self) -> &CompletionRule {
        &self.completion
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Event {
    NodeStarted {
        node_id: String,
    },
    NodeCompleted {
        node_id: String,
    },
    ArtifactCreated {
        artifact: Artifact,
    },
    BoardPatched {
        board_id: String,
        version: u64,
    },
    RouteMatched {
        route_id: String,
        source_artifact_id: Option<String>,
    },
    EffectRecorded {
        node_id: String,
        effect_key: String,
        fields: BTreeMap<String, String>,
    },
    FlowApplied {
        run_id: String,
        lock_id: String,
        content_hash: String,
        mode: String,
    },
}

impl Default for Event {
    fn default() -> Self {
        Self::NodeStarted {
            node_id: String::new(),
        }
    }
}

fn artifact_fingerprint(
    id: &str,
    name: &str,
    schema: &str,
    payload: &BTreeMap<String, String>,
) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325;

    write_fingerprint_segment(&mut hash, id);
    write_fingerprint_segment(&mut hash, name);
    write_fingerprint_segment(&mut hash, schema);

    for (key, value) in payload {
        write_fingerprint_segment(&mut hash, key);
        write_fingerprint_segment(&mut hash, value);
    }

    format!("{hash:016x}")
}

fn write_fingerprint_segment(hash: &mut u64, segment: &str) {
    for byte in segment.len().to_le_bytes() {
        write_fingerprint_byte(hash, byte);
    }
    for byte in segment.as_bytes() {
        write_fingerprint_byte(hash, *byte);
    }
}

fn write_fingerprint_byte(hash: &mut u64, byte: u8) {
    *hash ^= u64::from(byte);
    *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
}
