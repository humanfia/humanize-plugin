use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::participant_binding::ParticipantBindingFile;
use crate::runtime::{self, TickBudget};

use super::protocol::DriverWire;
use super::{
    DriverFailure, RuntimeDriverService, StoredPane, activation_status_name, required_string,
    route_tick_input, stop_decision_kind_name,
};

pub(super) const DEFAULT_STOP_ATTEMPT_LIMIT: u32 = 3;
pub(super) const MAX_STOP_ATTEMPT_LIMIT: u32 = 8;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct ParticipantBinding {
    pub(super) run_id: String,
    pub(super) activation_id: String,
    pub(super) allocation_generation: u64,
    pub(super) pane_id: String,
    pub(super) readiness_nonce: String,
    pub(super) handle: String,
    pub(super) credential_hash: String,
    pub(super) binding_file: String,
    pub(super) native_session_id: Option<String>,
    pub(super) platform: Option<String>,
    pub(super) source: Option<String>,
    pub(super) exit_status: Option<i32>,
}

impl ParticipantBinding {
    pub(super) fn key(&self) -> (String, u64) {
        (self.activation_id.clone(), self.allocation_generation)
    }
}

impl RuntimeDriverService {
    pub(super) fn ensure_participant_started(
        &mut self,
        activation_id: &str,
        pane: &StoredPane,
        readiness_nonce: &str,
    ) -> Result<ParticipantBinding, DriverFailure> {
        let key = (activation_id.to_string(), pane.allocation_generation);
        if let Some(binding) = self.participant_bindings.get(&key) {
            if binding.pane_id != pane.pane_id || binding.readiness_nonce != readiness_nonce {
                return Err(DriverFailure::new(
                    "participant_binding_conflict",
                    "participant binding does not match the current allocation",
                ));
            }
            self.participant_binding_secret(binding)?;
            return Ok(binding.clone());
        }
        let handle = random_private_identity()?;
        let credential = random_private_identity()?;
        let binding_file = format!("bindings/{handle}.json");
        let binding = ParticipantBinding {
            run_id: self.config.run_id.clone(),
            activation_id: activation_id.to_string(),
            allocation_generation: pane.allocation_generation,
            pane_id: pane.pane_id.clone(),
            readiness_nonce: readiness_nonce.to_string(),
            handle: handle.clone(),
            credential_hash: participant_credential_hash(&credential),
            binding_file: binding_file.clone(),
            native_session_id: None,
            platform: None,
            source: None,
            exit_status: None,
        };
        let binding_path = self.participant_binding_path(&binding)?;
        self.ensure_participant_binding_directory()?;
        ParticipantBindingFile::new(
            binding.run_id.clone(),
            binding.activation_id.clone(),
            binding.allocation_generation,
            binding.pane_id.clone(),
            binding.readiness_nonce.clone(),
            handle,
            credential,
        )
        .with_runs_root(self.config.runs_root.clone())
        .write(&binding_path)
        .map_err(|_| {
            DriverFailure::new(
                "participant_binding_persistence_failed",
                "private participant binding could not be persisted",
            )
        })?;
        if let Err(err) = self.append_driver_event(
            "participant_started",
            json!({
                "binding": binding,
                "stop_attempt_limit": self
                    .driver
                    .runtime()
                    .state()
                    .stop_attempt_limit(&self.config.run_id)
                    .unwrap_or(DEFAULT_STOP_ATTEMPT_LIMIT)
            }),
        ) {
            let _ = super::storage::remove_regular_file(&binding_path);
            return Err(err);
        }
        self.participant_bindings.insert(key, binding.clone());
        Ok(binding)
    }

    pub(super) fn bind_participant(&mut self, request: &Value) -> Result<Value, DriverFailure> {
        let activation_id = required_string(request, "activation_id")?;
        let allocation_generation = request
            .get("allocation_generation")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                DriverFailure::new("malformed_request", "allocation_generation is required")
            })?;
        let pane_id = required_string(request, "pane_id")?;
        let readiness_nonce = required_string(request, "readiness_nonce")?;
        let handle = required_string(request, "participant_handle")?;
        let credential = required_string(request, "participant_credential")?;
        let native_session_id = required_nonempty(request, "native_session_id")?;
        let platform = required_nonempty(request, "platform")?;
        let source = required_nonempty(request, "source")?;
        let key = (activation_id.to_string(), allocation_generation);
        let existing = self
            .participant_bindings
            .get(&key)
            .cloned()
            .ok_or_else(|| {
                DriverFailure::new(
                    "participant_binding_not_found",
                    "participant allocation binding was not started by the driver",
                )
            })?;
        self.validate_exact_binding(&existing, pane_id, readiness_nonce, handle, credential)?;

        let already_bound = match (
            existing.native_session_id.as_deref(),
            existing.platform.as_deref(),
            existing.source.as_deref(),
        ) {
            (None, None, None) => false,
            (Some(bound_session), Some(bound_platform), Some(bound_source))
                if bound_session == native_session_id
                    && bound_platform == platform
                    && bound_source == source =>
            {
                true
            }
            _ => {
                return Err(DriverFailure::new(
                    "participant_binding_conflict",
                    "participant allocation is already bound to a different native session",
                ));
            }
        };

        if !already_bound {
            self.append_driver_event(
                "participant_bound",
                json!({
                    "activation_id": activation_id,
                    "allocation_generation": allocation_generation,
                    "native_session_id": native_session_id,
                    "platform": platform,
                    "source": source
                }),
            )?;
            let binding = self
                .participant_bindings
                .get_mut(&key)
                .expect("validated participant binding should remain present");
            binding.native_session_id = Some(native_session_id.to_string());
            binding.platform = Some(platform.to_string());
            binding.source = Some(source.to_string());
        }

        self.reconcile_participant_publications()?;
        let actuation = if runtime::scheduling_enabled(
            self.driver.runtime().state(),
            &self.config.run_id,
            runtime::SchedulingIntent::Explicit,
        ) {
            self.actuate_activations(&[activation_id.to_string()])?
        } else {
            super::actuation::DriverActuation::default()
        };
        if actuation.requires_pause() {
            self.pause_for_reconciliation()?;
        }
        Ok(self.with_authority_fields(json!({
            "ok": true,
            "run_id": self.config.run_id,
            "participant_bound": true,
            "idempotent": already_bound,
            "actuation": actuation.to_json()
        })))
    }

    pub(super) fn participant_readiness(
        &self,
        activation_id: &str,
        pane_id: &str,
        allocation_generation: u64,
    ) -> Option<Value> {
        let binding = self
            .participant_bindings
            .get(&(activation_id.to_string(), allocation_generation))?;
        let native_session_id = binding.native_session_id.as_deref()?;
        if binding.pane_id != pane_id {
            return None;
        }
        Some(json!({
            "status": "ready",
            "platform": binding.platform,
            "source": binding.source,
            "native_session_id": native_session_id,
            "allocation_generation": allocation_generation
        }))
    }

    pub(super) fn participant_status_projection(&self, activation_id: &str) -> Value {
        let binding = self
            .participant_bindings
            .values()
            .filter(|binding| binding.activation_id == activation_id)
            .max_by_key(|binding| binding.allocation_generation);
        match binding {
            Some(binding) => json!({
                "started": true,
                "bound": binding.native_session_id.is_some(),
                "platform": binding.platform,
                "source": binding.source,
                "native_session_id": binding.native_session_id,
                "allocation_generation": binding.allocation_generation,
                "exited": binding.exit_status.is_some(),
                "exit_status": binding.exit_status
            }),
            None => Value::Null,
        }
    }

    pub(super) fn authorize_participant_tool(
        &self,
        wire: DriverWire,
        request: &Value,
    ) -> Result<Option<String>, DriverFailure> {
        let handle = request.get("participant_handle").and_then(Value::as_str);
        let credential = request
            .get("participant_credential")
            .and_then(Value::as_str);
        if handle.is_none() && credential.is_none() {
            return Ok(None);
        }
        let (Some(handle), Some(credential)) = (handle, credential) else {
            return Err(participant_unauthorized());
        };
        if !matches!(
            wire,
            DriverWire::Context
                | DriverWire::DeliverArtifact
                | DriverWire::RecordEffect
                | DriverWire::ValidateStop
        ) {
            return Err(DriverFailure::new(
                "participant_scope_denied",
                "driver operation is outside the participant tool scope",
            ));
        }
        let activation_id = required_string(request, "activation_id")?;
        let pane = self
            .tmux
            .as_ref()
            .and_then(|tmux| tmux.panes.get(activation_id))
            .ok_or_else(participant_unauthorized)?;
        let binding = self
            .participant_bindings
            .get(&(activation_id.to_string(), pane.allocation_generation))
            .ok_or_else(participant_unauthorized)?;
        let secret = self.participant_binding_secret(binding)?;
        if binding.pane_id != pane.pane_id
            || binding.handle != handle
            || secret.credential != credential
            || binding.native_session_id.is_none()
        {
            return Err(participant_unauthorized());
        }
        Ok(Some(activation_id.to_string()))
    }

    pub(super) fn participant_stop(&mut self, request: &Value) -> Result<Value, DriverFailure> {
        let binding = self.authorize_participant_hook(request)?;
        let invocation_id = required_nonempty(request, "invocation_id")?;
        if let Some(decision) =
            self.stop_decision_for_invocation(&binding.activation_id, invocation_id)
        {
            return Ok(self.stop_response(&binding.activation_id, &decision, true));
        }
        let reason = request
            .get("reason")
            .and_then(Value::as_str)
            .filter(|reason| !reason.trim().is_empty())
            .unwrap_or("participant requested stop");
        let mut observation = runtime::StopObservation::new(reason);
        observation.invocation_id = Some(invocation_id.to_string());
        let stop_attempt_limit = self
            .driver
            .runtime()
            .state()
            .stop_attempt_limit(&self.config.run_id)
            .unwrap_or(DEFAULT_STOP_ATTEMPT_LIMIT);
        let input = route_tick_input(self.route_locks())
            .with_budget(TickBudget {
                stop_validation_attempt_limit: stop_attempt_limit,
                stop_validations_per_tick: 1,
            })
            .with_stop_observation(
                self.config.run_id.clone(),
                binding.activation_id.clone(),
                observation,
            );
        let mut next_driver = self.driver.clone();
        let report = next_driver.tick(input);
        let decision = report.stop_decisions.first().cloned().ok_or_else(|| {
            DriverFailure::new(
                "stop_decision_unavailable",
                "runtime did not produce a participant stop decision",
            )
        })?;
        self.commit_runtime_with_publication(next_driver, &report.route_decisions)?;
        Ok(self.stop_response(&binding.activation_id, &decision, false))
    }

    pub(super) fn participant_exited(&mut self, request: &Value) -> Result<Value, DriverFailure> {
        let activation_id = required_string(request, "activation_id")?;
        let handle = required_string(request, "participant_handle")?;
        let credential = required_string(request, "participant_credential")?;
        let exit_status = request
            .get("exit_status")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| DriverFailure::new("malformed_request", "exit_status is required"))?;
        let mut key = None;
        for (candidate, binding) in &self.participant_bindings {
            if binding.activation_id == activation_id && binding.handle == handle {
                let credential_matches = if binding.exit_status.is_some() {
                    binding.credential_hash == participant_credential_hash(credential)
                } else {
                    self.participant_binding_secret(binding)?.credential == credential
                };
                if credential_matches {
                    key = Some(candidate.clone());
                    break;
                }
            }
        }
        let key = key.ok_or_else(participant_unauthorized)?;
        let existing = self
            .participant_bindings
            .get(&key)
            .cloned()
            .expect("located participant binding should remain present");
        if let Some(recorded) = existing.exit_status {
            if recorded != exit_status {
                return Err(DriverFailure::new(
                    "participant_exit_conflict",
                    "participant exit status conflicts with the durable observation",
                ));
            }
            self.reconcile_participant_publications()?;
            return Ok(self.with_authority_fields(json!({
                "ok": true,
                "run_id": self.config.run_id,
                "participant_exited": true,
                "idempotent": true,
                "response_ack_required": true
            })));
        }
        let current = self
            .tmux
            .as_ref()
            .and_then(|tmux| tmux.panes.get(activation_id));
        if current.is_none_or(|pane| {
            pane.pane_id != existing.pane_id
                || pane.allocation_generation != existing.allocation_generation
        }) {
            return Err(participant_unauthorized());
        }
        let mut next_driver = self.driver.clone();
        next_driver
            .record_participant_exit(
                &self.config.run_id,
                activation_id,
                existing.allocation_generation,
                exit_status,
            )
            .map_err(DriverFailure::from_runtime)?;
        self.commit_runtime(next_driver)?;
        self.participant_bindings
            .get_mut(&key)
            .expect("validated participant binding should remain present")
            .exit_status = Some(exit_status);
        self.reconcile_participant_publications()?;
        Ok(self.with_authority_fields(json!({
            "ok": true,
            "run_id": self.config.run_id,
            "participant_exited": true,
            "idempotent": false,
            "response_ack_required": true
        })))
    }

    pub(super) fn exited_participant_binding(&self, request: &Value) -> Option<ParticipantBinding> {
        let activation_id = request.get("activation_id").and_then(Value::as_str)?;
        let handle = request.get("participant_handle").and_then(Value::as_str)?;
        self.participant_bindings
            .values()
            .find(|binding| {
                binding.run_id == self.config.run_id
                    && binding.activation_id == activation_id
                    && binding.handle == handle
                    && binding.exit_status.is_some()
            })
            .cloned()
    }

    pub(super) fn reconcile_exited_participant(
        &mut self,
        expected: &ParticipantBinding,
    ) -> Result<(), DriverFailure> {
        if expected.run_id != self.config.run_id || expected.exit_status.is_none() {
            return Ok(());
        }
        let Some(binding) = self.participant_bindings.get(&expected.key()) else {
            return Ok(());
        };
        if binding.run_id != expected.run_id
            || binding.activation_id != expected.activation_id
            || binding.allocation_generation != expected.allocation_generation
            || binding.handle != expected.handle
            || binding.pane_id != expected.pane_id
            || binding.exit_status != expected.exit_status
        {
            return Ok(());
        }
        let Some(pane) = self
            .tmux
            .as_ref()
            .and_then(|tmux| tmux.panes.get(&binding.activation_id))
        else {
            return Ok(());
        };
        if pane.pane_id != binding.pane_id
            || pane.allocation_generation != binding.allocation_generation
        {
            return Ok(());
        }
        let terminal = self
            .driver
            .runtime()
            .state()
            .activations
            .get(&(self.config.run_id.clone(), binding.activation_id.clone()))
            .is_some_and(|activation| {
                matches!(
                    activation.status,
                    runtime::ActivationStatus::Blocked
                        | runtime::ActivationStatus::Completed
                        | runtime::ActivationStatus::Failed
                        | runtime::ActivationStatus::Cancelled
                )
            });
        if !terminal {
            return Ok(());
        }
        let activation_id = binding.activation_id.clone();
        self.finalize_activation_pane(&activation_id, "participant_exited")?;
        Ok(())
    }

    pub(super) fn reconcile_exited_participants(&mut self) -> Result<(), DriverFailure> {
        let bindings = self
            .participant_bindings
            .values()
            .filter(|binding| binding.exit_status.is_some())
            .cloned()
            .collect::<Vec<_>>();
        for binding in bindings {
            self.reconcile_exited_participant(&binding)?;
        }
        Ok(())
    }

    fn authorize_participant_hook(
        &self,
        request: &Value,
    ) -> Result<ParticipantBinding, DriverFailure> {
        let activation_id = required_string(request, "activation_id")?;
        let handle = required_string(request, "participant_handle")?;
        let credential = required_string(request, "participant_credential")?;
        let native_session_id = required_string(request, "native_session_id")?;
        let pane = self
            .tmux
            .as_ref()
            .and_then(|tmux| tmux.panes.get(activation_id))
            .ok_or_else(participant_unauthorized)?;
        let binding = self
            .participant_bindings
            .get(&(activation_id.to_string(), pane.allocation_generation))
            .cloned()
            .ok_or_else(participant_unauthorized)?;
        let secret = self.participant_binding_secret(&binding)?;
        if binding.pane_id != pane.pane_id
            || binding.handle != handle
            || secret.credential != credential
            || binding.native_session_id.as_deref() != Some(native_session_id)
        {
            return Err(participant_unauthorized());
        }
        Ok(binding)
    }

    fn stop_decision_for_invocation(
        &self,
        activation_id: &str,
        invocation_id: &str,
    ) -> Option<runtime::StopDecision> {
        let mut observed = false;
        for event in self.driver.runtime().events() {
            match &event.payload {
                runtime::EventPayload::StopObserved {
                    run_id,
                    activation_id: observed_activation_id,
                    observation,
                } if run_id == &self.config.run_id
                    && observed_activation_id == activation_id
                    && observation.invocation_id.as_deref() == Some(invocation_id) =>
                {
                    observed = true;
                }
                runtime::EventPayload::StopDecision {
                    run_id,
                    activation_id: decided_activation_id,
                    decision,
                } if observed
                    && run_id == &self.config.run_id
                    && decided_activation_id == activation_id =>
                {
                    return Some(decision.clone());
                }
                _ => {}
            }
        }
        None
    }

    fn stop_response(
        &self,
        activation_id: &str,
        decision: &runtime::StopDecision,
        idempotent: bool,
    ) -> Value {
        let activation_status = self
            .driver
            .runtime()
            .state()
            .activations
            .get(&(self.config.run_id.clone(), activation_id.to_string()))
            .map(|activation| activation_status_name(activation.status));
        let hook_action = match decision.kind {
            runtime::StopDecisionKind::Allow | runtime::StopDecisionKind::Block => "allow",
            runtime::StopDecisionKind::Deny | runtime::StopDecisionKind::Yield => "deny",
        };
        self.with_authority_fields(json!({
            "ok": true,
            "run_id": self.config.run_id,
            "decision": stop_decision_kind_name(decision.kind),
            "hook_action": hook_action,
            "attempt": decision.attempt,
            "missing_artifacts": decision.missing_artifacts,
            "missing_effects": decision.missing_effects,
            "reason": decision.reason,
            "activation_status": activation_status,
            "idempotent": idempotent
        }))
    }

    fn validate_exact_binding(
        &self,
        binding: &ParticipantBinding,
        pane_id: &str,
        readiness_nonce: &str,
        handle: &str,
        credential: &str,
    ) -> Result<(), DriverFailure> {
        let current = self
            .tmux
            .as_ref()
            .and_then(|tmux| tmux.panes.get(&binding.activation_id));
        let secret = self.participant_binding_secret(binding)?;
        if binding.run_id != self.config.run_id
            || binding.pane_id != pane_id
            || binding.readiness_nonce != readiness_nonce
            || binding.handle != handle
            || secret.credential != credential
            || current.is_none_or(|pane| {
                pane.pane_id != pane_id
                    || pane.allocation_generation != binding.allocation_generation
            })
        {
            return Err(DriverFailure::new(
                "participant_binding_mismatch",
                "participant binding does not match the current driver-owned allocation",
            ));
        }
        Ok(())
    }

    pub(super) fn participant_binding_path(
        &self,
        binding: &ParticipantBinding,
    ) -> Result<PathBuf, DriverFailure> {
        let path = Path::new(&binding.binding_file);
        let expected_file = format!("{}.json", binding.handle);
        let mut components = path.components();
        let valid = matches!(components.next(), Some(Component::Normal(value)) if value == "bindings")
            && matches!(components.next(), Some(Component::Normal(value)) if value == OsStr::new(&expected_file))
            && components.next().is_none();
        if !valid {
            return Err(DriverFailure::new(
                "participant_binding_invalid",
                "participant binding file identity is invalid",
            ));
        }
        Ok(self.private_run_root.join(path))
    }

    fn participant_binding_secret(
        &self,
        binding: &ParticipantBinding,
    ) -> Result<ParticipantBindingFile, DriverFailure> {
        let secret = ParticipantBindingFile::read(&self.participant_binding_path(binding)?)
            .map_err(|_| {
                DriverFailure::new(
                    "participant_binding_invalid",
                    "private participant binding is unavailable",
                )
            })?;
        if secret.run_id != binding.run_id
            || secret.activation_id != binding.activation_id
            || secret.allocation_generation != binding.allocation_generation
            || secret.pane_id != binding.pane_id
            || secret.readiness_nonce != binding.readiness_nonce
            || secret.handle != binding.handle
            || secret.runs_root != self.config.runs_root
        {
            return Err(DriverFailure::new(
                "participant_binding_invalid",
                "private participant binding does not match its durable identity",
            ));
        }
        Ok(secret)
    }

    fn ensure_participant_binding_directory(&self) -> Result<(), DriverFailure> {
        let path = self.private_run_root.join("bindings");
        let created = !path.exists();
        crate::private_state::ensure_private_directory(&self.private_run_root).map_err(|_| {
            DriverFailure::new(
                "participant_binding_persistence_failed",
                "private participant binding directory could not be created",
            )
        })?;
        crate::private_state::ensure_private_directory(&path).map_err(|_| {
            DriverFailure::new(
                "participant_binding_persistence_failed",
                "private participant binding directory is invalid",
            )
        })?;
        if created {
            File::open(&self.private_run_root)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| {
                    DriverFailure::new(
                        "participant_binding_persistence_failed",
                        "private participant binding directory was not durable",
                    )
                })?;
        }
        Ok(())
    }

    pub(super) fn retire_participant_binding(
        &mut self,
        activation_id: &str,
        allocation_generation: u64,
    ) -> Result<(), DriverFailure> {
        let Some(binding) = self
            .participant_bindings
            .get(&(activation_id.to_string(), allocation_generation))
            .cloned()
        else {
            return Ok(());
        };
        if binding.exit_status.is_some() {
            return Ok(());
        }
        let path = self.participant_binding_path(&binding)?;
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                self.participant_binding_secret(&binding)?;
                fs::remove_file(&path).map_err(|_| {
                    DriverFailure::new(
                        "participant_binding_cleanup_failed",
                        "private participant binding could not be removed",
                    )
                })?;
                File::open(path.parent().expect("binding path has a parent"))
                    .and_then(|directory| directory.sync_all())
                    .map_err(|_| {
                        DriverFailure::new(
                            "participant_binding_cleanup_failed",
                            "private participant binding removal was not durable",
                        )
                    })?;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(DriverFailure::new(
                    "participant_binding_cleanup_failed",
                    "private participant binding could not be inspected",
                ));
            }
        }
        let bindings_dir = self.private_run_root.join("bindings");
        if fs::read_dir(&bindings_dir).is_ok_and(|mut entries| entries.next().is_none()) {
            fs::remove_dir(&bindings_dir).map_err(|_| {
                DriverFailure::new(
                    "participant_binding_cleanup_failed",
                    "empty participant binding directory could not be removed",
                )
            })?;
            File::open(&self.private_run_root)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| {
                    DriverFailure::new(
                        "participant_binding_cleanup_failed",
                        "participant binding directory removal was not durable",
                    )
                })?;
        }
        Ok(())
    }
}

pub(super) fn project_participant_exits(
    events: &[runtime::Event],
    bindings: &mut BTreeMap<(String, u64), ParticipantBinding>,
) -> io::Result<()> {
    for event in events {
        let runtime::EventPayload::ParticipantExited {
            activation_id,
            allocation_generation,
            exit_status,
            ..
        } = &event.payload
        else {
            continue;
        };
        let binding = bindings
            .get_mut(&(activation_id.clone(), *allocation_generation))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "participant exit event has no started binding",
                )
            })?;
        if binding
            .exit_status
            .is_some_and(|recorded| recorded != *exit_status)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "participant exit event conflicts with its binding projection",
            ));
        }
        binding.exit_status = Some(*exit_status);
    }
    Ok(())
}

fn participant_unauthorized() -> DriverFailure {
    DriverFailure::new(
        "participant_unauthorized",
        "participant binding credential is invalid for the current allocation",
    )
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

fn random_private_identity() -> Result<String, DriverFailure> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|err| DriverFailure::new("randomness_failed", err.to_string()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn participant_credential_hash(credential: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(credential.as_bytes()))
}
