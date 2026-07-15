use serde_json::{Value, json};

use crate::adapters::tmux::TmuxActivationMetadata;
use crate::input_ledger::machine_input_payload_hash;
use crate::run_assets::{HookFactDetail, HookFactInput};
use crate::runtime;

use super::delivery::DELIVERY_ROLE_PARTICIPANT_MESSAGE;
use super::{DriverFailure, RuntimeDriverService, required_string};

const HOOK_ID_MAX_BYTES: usize = 128;
const SOURCE_NATIVE_ID_MAX_BYTES: usize = 256;
const HOOK_PAYLOAD_MAX_BYTES: usize = 64 * 1024;

impl RuntimeDriverService {
    pub(super) fn record_hook_fact(&mut self, request: &Value) -> Result<Value, DriverFailure> {
        let session_id = required_nonempty(request, "session_id")?;
        validate_bounded_id("session_id", session_id, HOOK_ID_MAX_BYTES)?;
        let hook = required_nonempty(request, "hook")?;
        validate_hook_name(hook)?;
        let source_native_id = match request.get("source_native_id") {
            Some(value) => value
                .as_str()
                .ok_or_else(|| {
                    DriverFailure::new("malformed_request", "source_native_id must be a string")
                })?
                .to_string(),
            None => format!("hook:{session_id}:{hook}"),
        };
        validate_bounded_id(
            "source_native_id",
            &source_native_id,
            SOURCE_NATIVE_ID_MAX_BYTES,
        )?;
        let activation_id = request
            .get("activation_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(activation_id) = activation_id.as_deref() {
            validate_bounded_id("activation_id", activation_id, HOOK_ID_MAX_BYTES)?;
            if !self
                .driver
                .runtime()
                .state()
                .activations
                .contains_key(&(self.config.run_id.clone(), activation_id.to_string()))
            {
                return Err(DriverFailure::new(
                    "activation_not_found",
                    "activation not found for driver-owned run",
                ));
            }
        }
        let payload = request.get("payload").cloned().unwrap_or(Value::Null);
        let payload_size = serde_json::to_vec(&payload)
            .map_err(|err| DriverFailure::new("malformed_request", err.to_string()))?
            .len();
        if payload_size > HOOK_PAYLOAD_MAX_BYTES {
            return Err(DriverFailure::new(
                "malformed_request",
                format!("payload exceeds {HOOK_PAYLOAD_MAX_BYTES} bytes"),
            ));
        }
        let causal_id = optional_bounded_id(request, "causal_id", SOURCE_NATIVE_ID_MAX_BYTES)?;
        let correlation_id =
            optional_bounded_id(request, "correlation_id", SOURCE_NATIVE_ID_MAX_BYTES)?;
        let detail = HookFactDetail::from_observation(hook, &payload)
            .map_err(|err| DriverFailure::new("malformed_request", err.to_string()))?;
        let mut manifest = self
            .run_asset_store
            .load_manifest(&self.config.run_id)
            .map_err(DriverFailure::from_run_asset)?;
        let record_generation = self
            .run_asset_store
            .record_hook_fact(
                &mut manifest,
                HookFactInput {
                    session_id: session_id.to_string(),
                    activation_id,
                    hook: hook.to_string(),
                    source_native_id: source_native_id.clone(),
                    detail,
                    causal_id,
                    correlation_id,
                },
            )
            .map_err(DriverFailure::from_run_asset)?;
        self.append_driver_event(
            "hook_fact_recorded",
            json!({
                "session_id": session_id,
                "hook": hook,
                "source_native_id": source_native_id,
                "record_generation": record_generation
            }),
        )?;
        let run_assets = self
            .run_asset_store
            .manifest_json(&manifest)
            .map_err(DriverFailure::from_run_asset)?;
        Ok(self.with_authority_fields(json!({
            "ok": true,
            "run_id": self.config.run_id,
            "session_id": session_id,
            "hook": hook,
            "record_generation": record_generation,
            "run_assets": run_assets
        })))
    }

    pub(super) fn send_message(&mut self, request: &Value) -> Result<Value, DriverFailure> {
        let activation_id = required_nonempty(request, "activation_id")?;
        let message_id = required_nonempty(request, "message_id")?;
        let text = required_nonempty(request, "text")?;
        let payload_hash = machine_input_payload_hash(text);

        if let Some(delivery) = self
            .ambiguous_deliveries
            .values()
            .find(|delivery| delivery.message_id.as_deref() == Some(message_id))
            .cloned()
        {
            self.validate_message_identity(
                &delivery.activation_id,
                &delivery.payload_hash,
                activation_id,
                &payload_hash,
            )?;
            return Ok(self.ambiguous_message_response(delivery.to_json()));
        }
        if let Some(delivery) = self
            .submitted_deliveries
            .values()
            .find(|delivery| delivery.message_id.as_deref() == Some(message_id))
            .cloned()
        {
            self.validate_message_identity(
                &delivery.activation_id,
                &delivery.payload_hash,
                activation_id,
                &payload_hash,
            )?;
            return Ok(self.with_authority_fields(json!({
                "ok": true,
                "run_id": self.config.run_id,
                "activation_id": activation_id,
                "message_id": message_id,
                "receipt": delivery.to_json(),
                "message_count": self.participant_message_count()
            })));
        }

        let activation = self
            .driver
            .runtime()
            .state()
            .activations
            .get(&(self.config.run_id.clone(), activation_id.to_string()))
            .cloned()
            .ok_or_else(|| {
                DriverFailure::new("activation_not_found", "target activation was not found")
            })?;
        if activation.status != runtime::ActivationStatus::Running {
            let mut failure = DriverFailure::new(
                "activation_not_running",
                "target activation must be running",
            );
            failure.extra = json!({ "activation_id": activation_id });
            return Err(failure);
        }
        let pane = self
            .tmux
            .as_ref()
            .and_then(|tmux| tmux.panes.get(activation_id))
            .cloned()
            .ok_or_else(|| {
                DriverFailure::new(
                    "pane_not_owned",
                    "target activation has no exact driver-owned pane",
                )
            })?;
        let metadata = TmuxActivationMetadata::new(
            pane.session_id.as_str(),
            self.config.run_id.as_str(),
            pane.window_name.as_str(),
            pane.window_id.as_str(),
            activation_id,
            pane.pane_id.as_str(),
        )
        .with_allocation_generation(pane.allocation_generation);
        let delivery = self.start_participant_message_delivery(
            activation_id,
            &pane.pane_id,
            message_id,
            text,
        )?;
        let transaction = match self
            .tmux_adapter
            .send_clean_input_transaction(&metadata, text)
        {
            Ok(transaction) => transaction,
            Err(_) => return Ok(self.ambiguous_message_response(delivery.to_json())),
        };
        if self
            .record_machine_input(DELIVERY_ROLE_PARTICIPANT_MESSAGE, transaction.record())
            .is_err()
        {
            return Ok(self.ambiguous_message_response(delivery.to_json()));
        }
        if self
            .finish_input_delivery(
                activation_id,
                DELIVERY_ROLE_PARTICIPANT_MESSAGE,
                Some(message_id),
                pane.allocation_generation,
                json!({
                    "pane_id": pane.pane_id,
                    "transaction_id": transaction.transaction_id()
                }),
            )
            .is_err()
        {
            return Ok(self.ambiguous_message_response(delivery.to_json()));
        }
        let receipt = self
            .submitted_deliveries
            .get(&delivery.key())
            .expect("submitted message delivery should be indexed")
            .to_json();
        Ok(self.with_authority_fields(json!({
            "ok": true,
            "run_id": self.config.run_id,
            "activation_id": activation_id,
            "message_id": message_id,
            "receipt": receipt,
            "message_count": self.participant_message_count()
        })))
    }

    fn validate_message_identity(
        &self,
        existing_activation_id: &str,
        existing_payload_hash: &str,
        activation_id: &str,
        payload_hash: &str,
    ) -> Result<(), DriverFailure> {
        if existing_activation_id == activation_id && existing_payload_hash == payload_hash {
            return Ok(());
        }
        Err(DriverFailure::new(
            "message_id_conflict",
            "message_id is already bound to a different target or payload",
        ))
    }

    fn ambiguous_message_response(&self, receipt: Value) -> Value {
        self.with_authority_fields(json!({
            "ok": false,
            "run_id": self.config.run_id,
            "error": {
                "code": "ambiguous_delivery",
                "message": "message delivery may have reached the receiver"
            },
            "receipt": receipt,
            "recovery": {
                "action": "resume_run",
                "resolution_required": true
            }
        }))
    }

    pub(super) fn participant_message_count(&self) -> usize {
        self.submitted_deliveries
            .values()
            .filter(|delivery| delivery.message_id.is_some())
            .count()
    }
}

fn required_nonempty<'a>(request: &'a Value, key: &'static str) -> Result<&'a str, DriverFailure> {
    let value = required_string(request, key)?;
    if value.trim().is_empty() {
        return Err(DriverFailure::new(
            "malformed_request",
            format!("{key} must be non-empty"),
        ));
    }
    Ok(value)
}

fn validate_bounded_id(label: &str, value: &str, max_bytes: usize) -> Result<(), DriverFailure> {
    if value.trim().is_empty() {
        return Err(DriverFailure::new(
            "malformed_request",
            format!("{label} must be non-empty"),
        ));
    }
    if value.len() > max_bytes {
        return Err(DriverFailure::new(
            "malformed_request",
            format!("{label} exceeds {max_bytes} bytes"),
        ));
    }
    Ok(())
}

fn validate_hook_name(hook: &str) -> Result<(), DriverFailure> {
    validate_bounded_id("hook", hook, HOOK_ID_MAX_BYTES)?;
    if matches!(hook, "compaction_pending" | "compaction_finished") || is_namespaced_hook(hook) {
        return Ok(());
    }
    Err(DriverFailure::new(
        "malformed_request",
        "hook must be a documented hook name or namespaced extension",
    ))
}

fn is_namespaced_hook(hook: &str) -> bool {
    let Some((namespace, name)) = hook.split_once('.') else {
        return false;
    };
    !namespace.is_empty()
        && !name.is_empty()
        && hook.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
}

fn optional_bounded_id(
    request: &Value,
    key: &'static str,
    max_bytes: usize,
) -> Result<Option<String>, DriverFailure> {
    let Some(value) = request.get(key) else {
        return Ok(None);
    };
    let value = value.as_str().ok_or_else(|| {
        DriverFailure::new("malformed_request", format!("{key} must be a string"))
    })?;
    validate_bounded_id(key, value, max_bytes)?;
    Ok(Some(value.to_string()))
}
