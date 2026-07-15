use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::flow::FlowLock;
use crate::run_assets::durable_fs::{SecureDir, SecureFsError, open_dir_path};

const PREPARED_FORMAT: &str = "humanize.flow_review_prepared.v1";
const DECISION_FORMAT: &str = "humanize.flow_review_decision.v1";
const PROJECTION_FORMAT: &str = "humanize.flow_review_projection.v1";
const MAC_KEY: &str = "review-mac.key";
const PREPARED_JSON: &str = "prepared.json";
const REVIEW_JSON: &str = "review.json";
const REVIEW_HTML: &str = "review.html";
const DECISION_JSON: &str = "decision.json";
static NEXT_STAGING: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    Rejected,
    Bypassed,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ReviewStatus {
    Pending,
    Approved,
    Rejected,
    Bypassed,
}

impl ReviewStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Bypassed => "bypassed",
        }
    }
}

impl From<ReviewDecision> for ReviewStatus {
    fn from(value: ReviewDecision) -> Self {
        match value {
            ReviewDecision::Approved => Self::Approved,
            ReviewDecision::Rejected => Self::Rejected,
            ReviewDecision::Bypassed => Self::Bypassed,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReviewStore {
    root: PathBuf,
}

impl ReviewStore {
    pub fn runtime_default() -> Result<Self, ReviewStoreError> {
        Ok(Self::new(user_state_root()?.join("reviews")))
    }

    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn prepare(
        &self,
        flow_lock: &FlowLock,
        snapshot: &Value,
        html: &str,
    ) -> Result<ReviewRecord, ReviewStoreError> {
        let root = open_dir_path(&self.root, true, true)?;
        let key = load_or_create_mac_key(&root)?;
        let review_id = review_id_for(flow_lock.id(), flow_lock.content_hash());
        let prepared = PreparedAuthority {
            format: PREPARED_FORMAT.to_string(),
            review_id: review_id.clone(),
            flow_lock: flow_lock.clone(),
        };
        let signed = SignedPreparedAuthority {
            mac: sign_record(&key, &prepared)?,
            record: prepared.clone(),
        };
        let prepared_bytes = pretty_json(&signed)?;

        let directory = match root.open_child_dir(OsStr::new(&review_id), true) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                create_review_directory(&root, &review_id, &prepared_bytes)?
            }
            Err(error) => return Err(error.into()),
        };
        let existing = load_prepared(&directory, &key, &review_id)?;
        if existing != prepared {
            return Err(ReviewStoreError::new("prepared review is immutable"));
        }

        let decision = load_decision(&directory, &key, &review_id)?;
        write_projection(&directory, &review_id, flow_lock, snapshot, html)?;
        Ok(self.record(prepared, decision))
    }

    pub fn load(&self, review_id: &str) -> Result<ReviewRecord, ReviewStoreError> {
        validate_review_id(review_id)?;
        let root = open_dir_path(&self.root, false, true)?;
        let key = load_mac_key(&root)?;
        let directory = root.open_child_dir(OsStr::new(review_id), true)?;
        let prepared = load_prepared(&directory, &key, review_id)?;
        let decision = load_decision(&directory, &key, review_id)?;
        Ok(self.record(prepared, decision))
    }

    pub fn decide(
        &self,
        review_id: &str,
        decision: ReviewDecision,
        reason: Option<&str>,
    ) -> Result<ReviewRecord, ReviewStoreError> {
        let current = self.load(review_id)?;
        let reason = reason.map(str::to_string);
        validate_decision_reason(decision, reason.as_deref())?;
        let document = DecisionAuthority {
            format: DECISION_FORMAT.to_string(),
            review_id: review_id.to_string(),
            decision,
            reason,
        };
        if let Some(existing) = current.decision.as_ref() {
            if existing == &document {
                return Ok(current);
            }
            return Err(ReviewStoreError::new("review decision is immutable"));
        }

        let root = open_dir_path(&self.root, false, true)?;
        let key = load_mac_key(&root)?;
        let directory = root.open_child_dir(OsStr::new(review_id), true)?;
        let signed = SignedDecisionAuthority {
            mac: sign_record(&key, &document)?,
            record: document.clone(),
        };
        let bytes = pretty_json(&signed)?;
        match directory.atomic_create_file(OsStr::new(DECISION_JSON), &bytes) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = load_decision(&directory, &key, review_id)?
                    .ok_or_else(|| ReviewStoreError::new("review decision is missing"))?;
                if existing != document {
                    return Err(ReviewStoreError::new("review decision is immutable"));
                }
            }
            Err(error) => return Err(error.into()),
        }
        self.load(review_id)
    }

    pub fn authorize(
        &self,
        review_id: &str,
        lock_id: &str,
        content_hash: &str,
    ) -> Result<ReviewRecord, ReviewStoreError> {
        let record = self.load(review_id)?;
        if record.flow_lock_id() != lock_id || record.content_hash() != content_hash {
            return Err(ReviewStoreError::new(
                "review does not match the requested flow lock",
            ));
        }
        match record.status() {
            ReviewStatus::Approved | ReviewStatus::Bypassed => Ok(record),
            ReviewStatus::Pending => Err(ReviewStoreError::new("flow review is pending")),
            ReviewStatus::Rejected => Err(ReviewStoreError::new("flow review was rejected")),
        }
    }

    fn record(
        &self,
        prepared: PreparedAuthority,
        decision: Option<DecisionAuthority>,
    ) -> ReviewRecord {
        let directory = self.root.join(&prepared.review_id);
        let review_path = directory.join(REVIEW_JSON);
        let document_path = directory.join(REVIEW_HTML);
        let document_uri = private_file_uri(&document_path)
            .expect("an already validated review path should produce a file URI");
        ReviewRecord {
            prepared,
            decision,
            directory,
            review_path,
            document_path,
            document_uri,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReviewRecord {
    prepared: PreparedAuthority,
    decision: Option<DecisionAuthority>,
    directory: PathBuf,
    review_path: PathBuf,
    document_path: PathBuf,
    document_uri: String,
}

impl ReviewRecord {
    pub fn review_id(&self) -> &str {
        &self.prepared.review_id
    }

    pub fn flow_lock_id(&self) -> &str {
        self.prepared.flow_lock.id()
    }

    pub fn content_hash(&self) -> &str {
        self.prepared.flow_lock.content_hash()
    }

    pub fn status(&self) -> ReviewStatus {
        self.decision
            .as_ref()
            .map(|decision| ReviewStatus::from(decision.decision))
            .unwrap_or(ReviewStatus::Pending)
    }

    pub fn reason(&self) -> Option<&str> {
        self.decision
            .as_ref()
            .and_then(|decision| decision.reason.as_deref())
    }

    pub fn review_directory(&self) -> &Path {
        &self.directory
    }

    pub fn review_json_path(&self) -> &Path {
        &self.review_path
    }

    pub fn document_path(&self) -> &Path {
        &self.document_path
    }

    pub fn document_uri(&self) -> &str {
        &self.document_uri
    }

    pub fn decision_path(&self) -> Option<PathBuf> {
        self.decision
            .as_ref()
            .map(|_| self.directory.join(DECISION_JSON))
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct PreparedAuthority {
    format: String,
    review_id: String,
    flow_lock: FlowLock,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct SignedPreparedAuthority {
    record: PreparedAuthority,
    mac: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct DecisionAuthority {
    format: String,
    review_id: String,
    decision: ReviewDecision,
    reason: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct SignedDecisionAuthority {
    record: DecisionAuthority,
    mac: String,
}

#[derive(Debug, Serialize)]
struct ReviewProjection<'a> {
    format: &'static str,
    derived_from: ProjectionSource<'a>,
    projection: &'a Value,
}

#[derive(Debug, Serialize)]
struct ProjectionSource<'a> {
    review_id: &'a str,
    flow_lock_id: &'a str,
    content_hash: &'a str,
}

#[derive(Debug)]
pub struct ReviewStoreError {
    message: String,
    kind: io::ErrorKind,
}

impl ReviewStoreError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: io::ErrorKind::InvalidData,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn kind(&self) -> io::ErrorKind {
        self.kind
    }
}

impl fmt::Display for ReviewStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ReviewStoreError {}

impl From<io::Error> for ReviewStoreError {
    fn from(error: io::Error) -> Self {
        Self {
            message: error.to_string(),
            kind: error.kind(),
        }
    }
}

impl From<serde_json::Error> for ReviewStoreError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<SecureFsError> for ReviewStoreError {
    fn from(error: SecureFsError) -> Self {
        Self {
            message: error.to_string(),
            kind: error.kind(),
        }
    }
}

fn create_review_directory(
    root: &SecureDir,
    review_id: &str,
    prepared: &[u8],
) -> Result<SecureDir, ReviewStoreError> {
    let staging_name = format!(
        ".{review_id}.staging-{}-{}",
        std::process::id(),
        NEXT_STAGING.fetch_add(1, Ordering::Relaxed)
    );
    let staging_name = OsString::from(staging_name);
    let staging = root.create_child_dir(&staging_name)?;
    let result: Result<SecureDir, ReviewStoreError> = (|| {
        staging.create_file(OsStr::new(PREPARED_JSON), prepared)?;
        staging.sync()?;
        drop(staging);
        root.rename_child_noreplace(&staging_name, OsStr::new(review_id))?;
        Ok(root.open_child_dir(OsStr::new(review_id), true)?)
    })();
    if result.is_err() {
        let _ = root.remove_child_tree(&staging_name);
    }
    match result {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            Ok(root.open_child_dir(OsStr::new(review_id), true)?)
        }
        result => result,
    }
}

fn write_projection(
    directory: &SecureDir,
    review_id: &str,
    flow_lock: &FlowLock,
    snapshot: &Value,
    html: &str,
) -> Result<(), ReviewStoreError> {
    let projection = ReviewProjection {
        format: PROJECTION_FORMAT,
        derived_from: ProjectionSource {
            review_id,
            flow_lock_id: flow_lock.id(),
            content_hash: flow_lock.content_hash(),
        },
        projection: snapshot,
    };
    let projection_bytes = pretty_json(&projection)?;
    let html = format!(
        "<!-- derived_from: {} {} {} -->\n{}",
        review_id,
        flow_lock.id(),
        flow_lock.content_hash(),
        html
    );
    directory.atomic_replace_file(OsStr::new(REVIEW_JSON), &projection_bytes)?;
    directory.atomic_replace_file(OsStr::new(REVIEW_HTML), html.as_bytes())?;
    Ok(())
}

fn load_prepared(
    directory: &SecureDir,
    key: &[u8; 32],
    review_id: &str,
) -> Result<PreparedAuthority, ReviewStoreError> {
    let bytes = directory.read_file(OsStr::new(PREPARED_JSON))?;
    let signed: SignedPreparedAuthority = serde_json::from_slice(&bytes)?;
    verify_record(key, &signed.record, &signed.mac)?;
    if signed.record.format != PREPARED_FORMAT
        || signed.record.review_id != review_id
        || review_id_for(
            signed.record.flow_lock.id(),
            signed.record.flow_lock.content_hash(),
        ) != review_id
    {
        return Err(ReviewStoreError::new("review identity mismatch"));
    }
    Ok(signed.record)
}

fn load_decision(
    directory: &SecureDir,
    key: &[u8; 32],
    review_id: &str,
) -> Result<Option<DecisionAuthority>, ReviewStoreError> {
    let bytes = match directory.read_file(OsStr::new(DECISION_JSON)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let signed: SignedDecisionAuthority = serde_json::from_slice(&bytes)?;
    verify_record(key, &signed.record, &signed.mac)?;
    if signed.record.format != DECISION_FORMAT || signed.record.review_id != review_id {
        return Err(ReviewStoreError::new("review decision identity mismatch"));
    }
    validate_decision_reason(signed.record.decision, signed.record.reason.as_deref())?;
    Ok(Some(signed.record))
}

fn load_or_create_mac_key(root: &SecureDir) -> Result<[u8; 32], ReviewStoreError> {
    match load_mac_key(root) {
        Ok(key) => Ok(key),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut key = [0_u8; 32];
            fill_random(&mut key)?;
            match root.atomic_create_file(OsStr::new(MAC_KEY), &key) {
                Ok(()) => Ok(key),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => load_mac_key(root),
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error),
    }
}

fn load_mac_key(root: &SecureDir) -> Result<[u8; 32], ReviewStoreError> {
    let bytes = root.read_file(OsStr::new(MAC_KEY))?;
    bytes
        .try_into()
        .map_err(|_| ReviewStoreError::new("review MAC key must be exactly 32 bytes"))
}

fn fill_random(bytes: &mut [u8]) -> Result<(), ReviewStoreError> {
    let mut offset = 0;
    while offset < bytes.len() {
        // SAFETY: the slice is valid writable memory for the requested remaining length.
        let read = unsafe {
            libc::getrandom(bytes[offset..].as_mut_ptr().cast(), bytes.len() - offset, 0)
        };
        if read < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if read == 0 {
            return Err(ReviewStoreError::new("system randomness returned no data"));
        }
        offset += read as usize;
    }
    Ok(())
}

fn sign_record<T: Serialize>(key: &[u8; 32], record: &T) -> Result<String, ReviewStoreError> {
    let bytes = serde_json::to_vec(record)?;
    Ok(hex(&hmac_sha256(key, &bytes)))
}

fn verify_record<T: Serialize>(
    key: &[u8; 32],
    record: &T,
    expected: &str,
) -> Result<(), ReviewStoreError> {
    let actual = sign_record(key, record)?;
    if !constant_time_equal(actual.as_bytes(), expected.as_bytes()) {
        return Err(ReviewStoreError::new("review authority MAC mismatch"));
    }
    Ok(())
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0_u8; 64];
    block[..key.len()].copy_from_slice(key);
    let mut inner_pad = [0x36_u8; 64];
    let mut outer_pad = [0x5c_u8; 64];
    for index in 0..block.len() {
        inner_pad[index] ^= block[index];
        outer_pad[index] ^= block[index];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner);
    outer.finalize().into()
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("writing to a string cannot fail");
    }
    output
}

fn pretty_json<T: Serialize>(value: &T) -> Result<Vec<u8>, ReviewStoreError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn user_state_root() -> Result<PathBuf, ReviewStoreError> {
    crate::state_path::user_state_root().map_err(ReviewStoreError::from)
}

fn review_id_for(lock_id: &str, content_hash: &str) -> String {
    let mut hasher = Sha256::new();
    for value in [lock_id, content_hash] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    format!("review_{:x}", hasher.finalize())
}

fn validate_review_id(review_id: &str) -> Result<(), ReviewStoreError> {
    if review_id.is_empty()
        || !review_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(ReviewStoreError::new("invalid review id"));
    }
    Ok(())
}

fn validate_decision_reason(
    decision: ReviewDecision,
    reason: Option<&str>,
) -> Result<(), ReviewStoreError> {
    if matches!(
        decision,
        ReviewDecision::Rejected | ReviewDecision::Bypassed
    ) && reason.is_none_or(|value| value.trim().is_empty())
    {
        return Err(ReviewStoreError::new(
            "reason is required for rejected or bypassed review decisions",
        ));
    }
    Ok(())
}

fn private_file_uri(path: &Path) -> Result<String, ReviewStoreError> {
    let absolute = std::path::absolute(path)?;
    let mut uri = String::from("file://");
    for byte in absolute.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            uri.push(char::from(*byte));
        } else {
            use std::fmt::Write as _;
            write!(uri, "%{byte:02X}").expect("writing to a string should not fail");
        }
    }
    Ok(uri)
}
