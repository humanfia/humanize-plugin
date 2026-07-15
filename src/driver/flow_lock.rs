use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::flow;
use crate::review::{ReviewStatus, ReviewStore};

use super::DriverFailure;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StoredFlowRevision {
    flow_lock: flow::FlowLock,
    review_id: String,
}

impl StoredFlowRevision {
    #[cfg(test)]
    pub(super) fn for_test(flow_lock: flow::FlowLock, review_id: impl Into<String>) -> Self {
        Self {
            flow_lock,
            review_id: review_id.into(),
        }
    }

    pub(super) fn from_preview_request(request: &Value) -> Result<Self, DriverFailure> {
        let value = request
            .get("flow_lock")
            .ok_or_else(|| DriverFailure::new("malformed_request", "flow_lock is required"))?;
        let flow_lock = serde_json::from_value::<flow::FlowLock>(value.clone())
            .map_err(|error| DriverFailure::new("invalid_flow_lock", error.to_string()))?;
        Ok(Self {
            flow_lock,
            review_id: String::new(),
        })
    }

    pub(super) fn from_request(
        request: &Value,
        review_store: &ReviewStore,
    ) -> Result<Self, DriverFailure> {
        let value = request
            .get("flow_lock")
            .ok_or_else(|| DriverFailure::new("malformed_request", "flow_lock is required"))?;
        let flow_lock = serde_json::from_value::<flow::FlowLock>(value.clone())
            .map_err(|error| DriverFailure::new("invalid_flow_lock", error.to_string()))?;
        let review_id = request
            .get("review_id")
            .or_else(|| request.get("reviewId"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| DriverFailure::new("review_missing", "review_id is required"))?
            .to_string();
        authorize_review(review_store, &review_id, &flow_lock)?;
        Ok(Self {
            flow_lock,
            review_id,
        })
    }

    pub(super) fn authorize(&self, review_store: &ReviewStore) -> Result<(), DriverFailure> {
        authorize_review(review_store, &self.review_id, &self.flow_lock)
    }

    pub(super) fn lock(&self) -> Result<flow::FlowLock, DriverFailure> {
        Ok(self.flow_lock.clone())
    }

    pub(super) fn lock_id(&self) -> &str {
        self.flow_lock.id()
    }

    pub(super) fn content_hash(&self) -> &str {
        self.flow_lock.content_hash()
    }

    pub(super) fn review_id(&self) -> &str {
        &self.review_id
    }

    pub(super) fn response_json(&self) -> Value {
        json!({
            "flow_lock": self.flow_lock,
            "review_id": self.review_id
        })
    }
}

fn authorize_review(
    review_store: &ReviewStore,
    review_id: &str,
    lock: &flow::FlowLock,
) -> Result<(), DriverFailure> {
    let review = review_store.load(review_id).map_err(|error| {
        DriverFailure::new(
            "review_invalid",
            format!("flow review load failed: {error}"),
        )
    })?;
    if review.flow_lock_id() != lock.id() || review.content_hash() != lock.content_hash() {
        return Err(DriverFailure::new(
            "review_binding_mismatch",
            "flow review does not match the canonical flow lock identity",
        ));
    }
    match review.status() {
        ReviewStatus::Approved | ReviewStatus::Bypassed => Ok(()),
        ReviewStatus::Pending => Err(DriverFailure::new(
            "review_pending",
            "flow review is pending",
        )),
        ReviewStatus::Rejected => Err(DriverFailure::new(
            "review_rejected",
            "flow review was rejected",
        )),
    }
}
