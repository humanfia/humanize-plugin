use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::input_ledger::machine_input_payload_hash;

use super::{DriverFailure, RuntimeDriverService, string_field};

pub(super) const DELIVERY_ROLE_AGENT_LAUNCH: &str = "agent_launch";
pub(super) const DELIVERY_ROLE_NODE_PROMPT: &str = "node_prompt";
pub(super) const DELIVERY_ROLE_PARTICIPANT_MESSAGE: &str = "participant_message";

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct AmbiguousDelivery {
    pub(super) activation_id: String,
    pub(super) pane_id: String,
    pub(super) allocation_generation: u64,
    pub(super) role: String,
    pub(super) message_id: Option<String>,
    pub(super) payload_hash: String,
    pub(super) started_event_sequence: u64,
    pub(super) reason: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct SubmittedDelivery {
    pub(super) activation_id: String,
    pub(super) pane_id: String,
    pub(super) allocation_generation: u64,
    pub(super) role: String,
    pub(super) message_id: Option<String>,
    pub(super) payload_hash: String,
    pub(super) started_event_sequence: u64,
}

impl From<&AmbiguousDelivery> for SubmittedDelivery {
    fn from(delivery: &AmbiguousDelivery) -> Self {
        Self {
            activation_id: delivery.activation_id.clone(),
            pane_id: delivery.pane_id.clone(),
            allocation_generation: delivery.allocation_generation,
            role: delivery.role.clone(),
            message_id: delivery.message_id.clone(),
            payload_hash: delivery.payload_hash.clone(),
            started_event_sequence: delivery.started_event_sequence,
        }
    }
}

impl AmbiguousDelivery {
    pub(super) fn from_payload(payload: &Value, event_sequence: u64) -> Option<Self> {
        Some(Self {
            activation_id: payload.get("activation_id")?.as_str()?.to_string(),
            pane_id: payload.get("pane_id")?.as_str()?.to_string(),
            allocation_generation: payload
                .get("allocation_generation")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            role: payload.get("role")?.as_str()?.to_string(),
            message_id: payload
                .get("message_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            payload_hash: payload.get("payload_hash")?.as_str()?.to_string(),
            started_event_sequence: payload
                .get("started_event_sequence")
                .and_then(Value::as_u64)
                .unwrap_or(event_sequence),
            reason: payload
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("submission_receipt_incomplete")
                .to_string(),
        })
    }

    pub(super) fn to_json(&self) -> Value {
        json!({
            "activation_id": self.activation_id,
            "pane_id": self.pane_id,
            "allocation_generation": self.allocation_generation,
            "role": self.role,
            "message_id": self.message_id,
            "payload_hash": self.payload_hash,
            "started_event_sequence": self.started_event_sequence,
            "reason": self.reason,
            "status": "ambiguous_delivery",
            "resolution_required": true
        })
    }

    pub(super) fn key(&self) -> (String, String) {
        delivery_key(
            &self.activation_id,
            &self.role,
            self.message_id.as_deref(),
            self.allocation_generation,
        )
    }
}

impl SubmittedDelivery {
    pub(super) fn to_json(&self) -> Value {
        json!({
            "activation_id": self.activation_id,
            "pane_id": self.pane_id,
            "allocation_generation": self.allocation_generation,
            "role": self.role,
            "message_id": self.message_id,
            "payload_hash": self.payload_hash,
            "started_event_sequence": self.started_event_sequence,
            "status": "submitted"
        })
    }
}

#[derive(Debug)]
pub(super) struct InputDeliveryResolution {
    started_event_sequence: u64,
    outcome: String,
    evidence: String,
}

pub(super) fn input_delivery_resolution_from_request(
    request: &Value,
) -> Result<Option<InputDeliveryResolution>, DriverFailure> {
    let Some(value) = request
        .get("delivery_resolution")
        .or_else(|| request.get("deliveryResolution"))
    else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| {
        DriverFailure::new("malformed_request", "delivery_resolution must be an object")
    })?;
    let started_event_sequence = object
        .get("started_event_sequence")
        .or_else(|| object.get("startedEventSequence"))
        .and_then(Value::as_u64)
        .filter(|sequence| *sequence > 0)
        .ok_or_else(|| {
            DriverFailure::new(
                "malformed_request",
                "delivery resolution started_event_sequence must be a positive integer",
            )
        })?;
    let outcome = string_field(object, "outcome")?.to_string();
    if !matches!(outcome.as_str(), "submitted" | "not_submitted") {
        return Err(DriverFailure::new(
            "malformed_request",
            "delivery resolution outcome must be submitted or not_submitted",
        ));
    }
    let evidence = string_field(object, "evidence")?.trim().to_string();
    if evidence.is_empty() {
        return Err(DriverFailure::new(
            "malformed_request",
            "delivery resolution evidence must be non-empty",
        ));
    }
    Ok(Some(InputDeliveryResolution {
        started_event_sequence,
        outcome,
        evidence,
    }))
}

impl RuntimeDriverService {
    pub(super) fn validate_input_delivery_resolution(
        &self,
        resolution: &InputDeliveryResolution,
    ) -> Result<(), DriverFailure> {
        self.delivery_for_resolution(resolution).map(|_| ())
    }

    pub(super) fn resolve_input_delivery(
        &mut self,
        resolution: InputDeliveryResolution,
    ) -> Result<Value, DriverFailure> {
        let delivery = self.delivery_for_resolution(&resolution)?;
        let key = delivery.key();
        match resolution.outcome.as_str() {
            "not_submitted" => {
                self.append_driver_event(
                    "input_delivery_not_submitted",
                    json!({
                        "activation_id": delivery.activation_id,
                        "pane_id": delivery.pane_id,
                        "allocation_generation": delivery.allocation_generation,
                        "role": delivery.role,
                        "message_id": delivery.message_id,
                        "started_event_sequence": delivery.started_event_sequence,
                        "evidence": resolution.evidence
                    }),
                )?;
                self.ambiguous_deliveries.remove(&key);
                self.submitted_deliveries.remove(&key);
            }
            "submitted" => {
                self.finish_input_delivery(
                    &delivery.activation_id,
                    &delivery.role,
                    delivery.message_id.as_deref(),
                    delivery.allocation_generation,
                    json!({
                        "pane_id": delivery.pane_id,
                        "resolution": "submitted",
                        "evidence": resolution.evidence
                    }),
                )?;
            }
            _ => {
                return Err(DriverFailure::new(
                    "malformed_request",
                    "delivery resolution outcome must be submitted or not_submitted",
                ));
            }
        }
        Ok(json!({
            "started_event_sequence": delivery.started_event_sequence,
            "activation_id": delivery.activation_id,
            "role": delivery.role,
            "message_id": delivery.message_id,
            "outcome": resolution.outcome,
            "evidence": resolution.evidence
        }))
    }

    fn delivery_for_resolution(
        &self,
        resolution: &InputDeliveryResolution,
    ) -> Result<AmbiguousDelivery, DriverFailure> {
        let matches = self
            .ambiguous_deliveries
            .values()
            .filter(|delivery| delivery.started_event_sequence == resolution.started_event_sequence)
            .cloned()
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            return Ok(matches[0].clone());
        }
        let mut failure = DriverFailure::new(
            "delivery_barrier_conflict",
            "delivery resolution does not match an active ambiguity barrier",
        );
        failure.extra = json!({
            "started_event_sequence": resolution.started_event_sequence,
            "active_started_event_sequences": self
                .ambiguous_deliveries
                .values()
                .map(|delivery| delivery.started_event_sequence)
                .collect::<Vec<_>>()
        });
        Err(failure)
    }

    pub(super) fn ambiguous_input_delivery(
        &self,
        activation_id: &str,
        role: &str,
        allocation_generation: u64,
    ) -> Option<AmbiguousDelivery> {
        self.ambiguous_deliveries
            .get(&delivery_key(
                activation_id,
                role,
                None,
                allocation_generation,
            ))
            .cloned()
    }

    pub(super) fn start_input_delivery(
        &mut self,
        activation_id: &str,
        pane_id: &str,
        role: &str,
        text: &str,
    ) -> Result<AmbiguousDelivery, DriverFailure> {
        self.start_input_delivery_with_message(activation_id, pane_id, role, None, text)
    }

    pub(super) fn start_participant_message_delivery(
        &mut self,
        activation_id: &str,
        pane_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<AmbiguousDelivery, DriverFailure> {
        self.start_input_delivery_with_message(
            activation_id,
            pane_id,
            DELIVERY_ROLE_PARTICIPANT_MESSAGE,
            Some(message_id),
            text,
        )
    }

    fn start_input_delivery_with_message(
        &mut self,
        activation_id: &str,
        pane_id: &str,
        role: &str,
        message_id: Option<&str>,
        text: &str,
    ) -> Result<AmbiguousDelivery, DriverFailure> {
        let allocation_generation = self
            .tmux
            .as_ref()
            .and_then(|tmux| tmux.panes.get(activation_id))
            .filter(|pane| pane.pane_id == pane_id)
            .map(|pane| pane.allocation_generation)
            .ok_or_else(|| {
                DriverFailure::new(
                    "pane_not_owned",
                    "input delivery requires the exact current pane allocation",
                )
            })?;
        let payload_hash = machine_input_payload_hash(text);
        self.append_driver_event(
            "input_delivery_started",
            json!({
                "activation_id": activation_id,
                "pane_id": pane_id,
                "allocation_generation": allocation_generation,
                "role": role,
                "message_id": message_id,
                "payload_hash": payload_hash,
                "reason": "submission_receipt_incomplete"
            }),
        )?;
        let delivery = AmbiguousDelivery {
            activation_id: activation_id.to_string(),
            pane_id: pane_id.to_string(),
            allocation_generation,
            role: role.to_string(),
            message_id: message_id.map(str::to_string),
            payload_hash,
            started_event_sequence: self.driver_event_count,
            reason: "submission_receipt_incomplete".to_string(),
        };
        let key = delivery.key();
        self.submitted_deliveries.remove(&key);
        self.ambiguous_deliveries.insert(key, delivery.clone());
        Ok(delivery)
    }

    pub(super) fn finish_input_delivery(
        &mut self,
        activation_id: &str,
        role: &str,
        message_id: Option<&str>,
        allocation_generation: u64,
        details: Value,
    ) -> Result<(), DriverFailure> {
        let event_kind = match role {
            DELIVERY_ROLE_AGENT_LAUNCH => "agent_launch_submitted",
            DELIVERY_ROLE_NODE_PROMPT => "prompt_submitted",
            DELIVERY_ROLE_PARTICIPANT_MESSAGE => "participant_message_submitted",
            _ => {
                return Err(DriverFailure::new(
                    "malformed_request",
                    "unknown input delivery role",
                ));
            }
        };
        let mut payload = match details {
            Value::Object(object) => object,
            _ => Map::new(),
        };
        let key = delivery_key(activation_id, role, message_id, allocation_generation);
        let delivery = self
            .ambiguous_deliveries
            .get(&key)
            .cloned()
            .ok_or_else(|| {
                DriverFailure::new(
                    "delivery_barrier_conflict",
                    "input submission does not match an active ambiguity barrier",
                )
            })?;
        payload.insert(
            "activation_id".to_string(),
            Value::String(activation_id.to_string()),
        );
        if let Some(message_id) = message_id {
            payload.insert(
                "message_id".to_string(),
                Value::String(message_id.to_string()),
            );
        }
        payload.insert(
            "started_event_sequence".to_string(),
            Value::from(delivery.started_event_sequence),
        );
        payload.insert(
            "allocation_generation".to_string(),
            Value::from(delivery.allocation_generation),
        );
        self.append_driver_event(event_kind, Value::Object(payload))?;
        self.ambiguous_deliveries.remove(&key);
        self.submitted_deliveries
            .insert(key, SubmittedDelivery::from(&delivery));
        if matches!(role, DELIVERY_ROLE_AGENT_LAUNCH | DELIVERY_ROLE_NODE_PROMPT) {
            self.agent_launch_submitted_activations
                .insert((activation_id.to_string(), allocation_generation));
        }
        if role == DELIVERY_ROLE_NODE_PROMPT {
            self.settled_actuation_activations
                .insert((activation_id.to_string(), allocation_generation));
        }
        Ok(())
    }

    pub(super) fn ambiguous_delivery_warning(
        &self,
        delivery: &AmbiguousDelivery,
        node_id: &str,
        driver: &str,
    ) -> Value {
        let mut warning = delivery.to_json();
        let object = warning
            .as_object_mut()
            .expect("ambiguous delivery warning must be an object");
        object.insert("node_id".to_string(), Value::String(node_id.to_string()));
        object.insert("driver".to_string(), Value::String(driver.to_string()));
        object.insert(
            "message".to_string(),
            Value::String(
                "delivery may have reached the receiver; explicit resolution is required before retry"
                    .to_string(),
            ),
        );
        warning
    }

    pub(super) fn install_released_delivery_barriers(
        &mut self,
        barriers: &[Value],
        event_sequence: u64,
    ) {
        for barrier in barriers {
            let Some(delivery) = AmbiguousDelivery::from_payload(barrier, event_sequence) else {
                continue;
            };
            let key = delivery.key();
            self.submitted_deliveries.remove(&key);
            self.ambiguous_deliveries.entry(key).or_insert(delivery);
        }
    }
}

pub(super) fn delivery_key(
    activation_id: &str,
    role: &str,
    message_id: Option<&str>,
    allocation_generation: u64,
) -> (String, String) {
    let role_key = match message_id {
        Some(message_id) => format!("{role}:{message_id}@{allocation_generation}"),
        None => format!("{role}@{allocation_generation}"),
    };
    (activation_id.to_string(), role_key)
}
