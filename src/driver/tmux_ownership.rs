use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::adapters::tmux::{
    TmuxActivationMetadata, TmuxPane, TmuxPanePresence, TmuxSession, TmuxWindow,
};
use crate::runtime;

use super::actuation::DriverActuation;
use super::{DriverFailure, RuntimeDriverService, maybe_crash_after_tmux_effect, string_field};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct TmuxPaneAllocationIntent {
    pub(super) operation_id: String,
    pub(super) activation_id: String,
    pub(super) allocation_generation: u64,
    pub(super) session_id: String,
    pub(super) window_name: String,
    pub(super) agent_command: String,
    #[serde(default)]
    pub(super) actuation: DriverTmuxActuationConfig,
    pub(super) existing_window_id: Option<String>,
}

impl TmuxPaneAllocationIntent {
    fn config(&self) -> DriverTmuxConfig {
        DriverTmuxConfig {
            session: self.session_id.clone(),
            window: self.window_name.clone(),
            agent_command: self.agent_command.clone(),
            actuation: self.actuation.clone(),
            existing_window_id: self.existing_window_id.clone(),
        }
    }
}

impl RuntimeDriverService {
    pub(super) fn bind_tmux_response(
        &mut self,
        request: &Value,
        activation_ids: &[String],
    ) -> Result<Value, DriverFailure> {
        let Some(mut config) = tmux_config_from_request(request)? else {
            return Ok(json!({
                "enabled": false,
                "panes": [],
                "actuation": DriverActuation::default().to_json()
            }));
        };
        if !runtime::scheduling_enabled(
            self.driver.runtime().state(),
            &self.config.run_id,
            runtime::SchedulingIntent::Explicit,
        ) {
            let warnings = self
                .ambiguous_deliveries
                .values()
                .map(super::delivery::AmbiguousDelivery::to_json)
                .collect();
            let panes = self
                .tmux
                .as_ref()
                .map(|tmux| {
                    tmux.panes
                        .iter()
                        .map(|(activation_id, pane)| pane.to_json(activation_id))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            return Ok(json!({
                "enabled": self.tmux.is_some(),
                "session_id": self.tmux.as_ref().map(|tmux| tmux.session_id.as_str()),
                "window_id": self.tmux.as_ref().map(|tmux| tmux.window_id.as_str()),
                "window_name": self.tmux.as_ref().map(|tmux| tmux.window_name.as_str()),
                "panes": panes,
                "actuation": DriverActuation::with_warnings(warnings).to_json()
            }));
        }
        if config.existing_window_id.is_none()
            && let Some(operator) = self.config.operator_pane.as_ref()
            && operator.session_id == config.session
            && operator.window_name == config.window
        {
            config.existing_window_id = Some(operator.window_id.clone());
        }
        let (panes, actuation) =
            match self.reconcile_post_commit_with_tmux_config(Some(config), activation_ids) {
                Ok(result) => result,
                Err(err) => return Err(self.bind_cleanup_failure(err)),
            };
        Ok(json!({
            "enabled": true,
            "session_id": self.tmux.as_ref().map(|tmux| tmux.session_id.as_str()),
            "window_id": self.tmux.as_ref().map(|tmux| tmux.window_id.as_str()),
            "window_name": self.tmux.as_ref().map(|tmux| tmux.window_name.as_str()),
            "panes": panes,
            "actuation": actuation.to_json()
        }))
    }

    fn bind_cleanup_failure(&mut self, err: DriverFailure) -> DriverFailure {
        let reported_panes = err.tmux_cleanup_panes();
        let mut release_outcomes = err.tmux_release_outcomes();
        let owned_before = self
            .tmux
            .as_ref()
            .map(|tmux| {
                tmux.panes
                    .iter()
                    .map(|(activation_id, pane)| pane.to_json(activation_id))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Err(cleanup_err) = self.finalize_terminal_node_panes("bind_failed") {
            release_outcomes.extend(cleanup_err.tmux_release_outcomes());
        }
        let owned_after = self
            .tmux
            .as_ref()
            .map(|tmux| {
                tmux.panes
                    .iter()
                    .map(|(activation_id, pane)| pane.to_json(activation_id))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let physically_released = release_outcomes
            .iter()
            .filter_map(|outcome| outcome.get("pane"))
            .cloned()
            .collect::<Vec<_>>();
        let mut cleanup_panes = reported_panes
            .into_iter()
            .filter(|pane| {
                (!owned_before.contains(pane) || owned_after.contains(pane))
                    && !physically_released.contains(pane)
            })
            .collect::<Vec<_>>();
        cleanup_panes.extend(
            owned_after
                .into_iter()
                .filter(|pane| !physically_released.contains(pane)),
        );
        err.with_tmux_release_outcomes(release_outcomes)
            .replacing_tmux_cleanup(cleanup_panes)
    }

    pub(super) fn release_tmux_panes(
        &mut self,
        activation_ids: &[String],
    ) -> Result<(), DriverFailure> {
        if activation_ids.is_empty() {
            return Ok(());
        }
        let delivery_barriers = self.released_delivery_barriers(activation_ids);
        self.append_driver_event(
            "tmux_panes_released",
            json!({
                "activation_ids": activation_ids,
                "delivery_barriers": delivery_barriers
            }),
        )?;
        self.install_released_delivery_barriers(&delivery_barriers, self.driver_event_count);
        if let Some(tmux) = self.tmux.as_mut() {
            for activation_id in activation_ids {
                tmux.panes.remove(activation_id);
                self.agent_launch_submitted_activations
                    .retain(|(submitted_activation_id, _)| {
                        submitted_activation_id != activation_id
                    });
                self.settled_actuation_activations
                    .retain(|(settled_activation_id, _)| settled_activation_id != activation_id);
                self.pending_pipe_captures.remove(activation_id);
                self.tmux_pipe_captures.remove(activation_id);
            }
        }
        Ok(())
    }

    fn ensure_tmux_for_activations(
        &mut self,
        config: DriverTmuxConfig,
        activation_ids: &[String],
    ) -> Result<Vec<Value>, DriverFailure> {
        let mut allocated = Vec::new();
        let requested_binding = DriverTmuxState {
            session_id: config.session.clone(),
            window_name: config.window.clone(),
            window_id: config.existing_window_id.clone().unwrap_or_default(),
            agent_command: config.agent_command.clone(),
            actuation: config.actuation.clone(),
            panes: BTreeMap::new(),
        };
        let should_replace_binding = self.tmux.as_ref().is_some_and(|tmux| {
            !requested_binding.window_id.is_empty()
                && (tmux.session_id != requested_binding.session_id
                    || tmux.window_id != requested_binding.window_id
                    || tmux.window_name != requested_binding.window_name
                    || tmux.agent_command != requested_binding.agent_command
                    || tmux.actuation != requested_binding.actuation)
        });
        if should_replace_binding {
            self.append_tmux_binding(&requested_binding)?;
            self.tmux = Some(requested_binding.clone());
        }

        if self.tmux.is_none() && !requested_binding.window_id.is_empty() {
            self.append_tmux_binding(&requested_binding)?;
            self.tmux = Some(requested_binding.clone());
        }

        self.reconcile_stale_tmux_panes(activation_ids)?;

        for activation_id in activation_ids {
            if self
                .tmux
                .as_ref()
                .is_some_and(|tmux| tmux.panes.contains_key(activation_id))
            {
                if let Some(pane) = self
                    .tmux
                    .as_ref()
                    .and_then(|tmux| tmux.panes.get(activation_id))
                {
                    allocated.push(pane.to_json(activation_id));
                }
                continue;
            }
            let stored = self.ensure_identified_tmux_pane(&config, activation_id)?;
            let allocation = stored.to_json(activation_id);
            allocated.push(allocation.clone());
        }
        Ok(allocated)
    }

    fn ensure_identified_tmux_pane(
        &mut self,
        config: &DriverTmuxConfig,
        activation_id: &str,
    ) -> Result<StoredPane, DriverFailure> {
        let intent = match self.pending_tmux_allocations.get(activation_id) {
            Some(intent) => intent.clone(),
            None => {
                let allocation_generation = self.next_allocation_generation(activation_id);
                let existing_window_id = self
                    .tmux
                    .as_ref()
                    .map(|tmux| tmux.window_id.clone())
                    .filter(|window_id| !window_id.is_empty())
                    .or_else(|| config.existing_window_id.clone());
                let intent = TmuxPaneAllocationIntent {
                    operation_id: tmux_allocation_operation_id(
                        &self.config.run_id,
                        activation_id,
                        allocation_generation,
                    ),
                    activation_id: activation_id.to_string(),
                    allocation_generation,
                    session_id: config.session.clone(),
                    window_name: config.window.clone(),
                    agent_command: config.agent_command.clone(),
                    actuation: config.actuation.clone(),
                    existing_window_id,
                };
                self.append_driver_event(
                    "tmux_pane_allocation_intent",
                    serde_json::to_value(&intent)
                        .map_err(|err| DriverFailure::new("persistence_failed", err.to_string()))?,
                )?;
                self.pending_tmux_allocations
                    .insert(activation_id.to_string(), intent.clone());
                intent
            }
        };

        let identified = self
            .tmux_adapter
            .find_identified_pane(
                &intent.session_id,
                &self.config.run_id,
                &intent.window_name,
                activation_id,
                &intent.operation_id,
            )
            .map_err(DriverFailure::from_tmux)?;
        let (window, pane) = match identified {
            Some(identified) => identified,
            None => self.create_identified_tmux_pane(&intent)?,
        };
        maybe_crash_after_tmux_effect("pane_created");
        let stored = StoredPane::from_tmux_pane(&window, &pane, intent.allocation_generation);
        let binding = DriverTmuxState {
            session_id: window.session_id().to_string(),
            window_name: window.name().to_string(),
            window_id: window.id().to_string(),
            agent_command: intent.agent_command.clone(),
            actuation: intent.actuation.clone(),
            panes: self
                .tmux
                .as_ref()
                .map(|tmux| tmux.panes.clone())
                .unwrap_or_default(),
        };
        if self.tmux.as_ref().is_none_or(|tmux| {
            tmux.session_id != binding.session_id
                || tmux.window_id != binding.window_id
                || tmux.window_name != binding.window_name
                || tmux.agent_command != binding.agent_command
                || tmux.actuation != binding.actuation
        }) {
            let cleanup = stored.to_json(activation_id);
            if let Err(err) = self.append_tmux_binding(&binding) {
                return Err(err.with_tmux_cleanup(vec![cleanup]));
            }
            self.tmux = Some(binding);
        }
        self.publish_tmux_pane(activation_id, stored.clone(), &intent.operation_id)?;
        Ok(stored)
    }

    fn create_identified_tmux_pane(
        &self,
        intent: &TmuxPaneAllocationIntent,
    ) -> Result<(TmuxWindow, TmuxPane), DriverFailure> {
        if let Some(window_id) = intent.existing_window_id.as_deref() {
            let window = TmuxWindow::new_named(
                &intent.session_id,
                &self.config.run_id,
                &intent.window_name,
                window_id,
            );
            let pane = self
                .tmux_adapter
                .split_pane_for_activation_identified(
                    &window,
                    &intent.activation_id,
                    &intent.operation_id,
                )
                .map_err(DriverFailure::from_tmux)?;
            return Ok((window, pane));
        }

        let session = TmuxSession::new(&intent.session_id);
        if self
            .tmux_adapter
            .has_session(&session)
            .map_err(DriverFailure::from_tmux)?
        {
            return self
                .tmux_adapter
                .create_window_named_with_pane_identified(
                    &session,
                    &self.config.run_id,
                    &intent.window_name,
                    &intent.activation_id,
                    &intent.operation_id,
                )
                .map_err(DriverFailure::from_tmux);
        }
        let (_, window, pane) = self
            .tmux_adapter
            .create_session_with_window_pane_identified(
                &intent.session_id,
                &self.config.run_id,
                &intent.window_name,
                &intent.activation_id,
                &intent.operation_id,
            )
            .map_err(DriverFailure::from_tmux)?;
        Ok((window, pane))
    }

    fn reconcile_stale_tmux_panes(
        &mut self,
        activation_ids: &[String],
    ) -> Result<(), DriverFailure> {
        let Some(tmux) = self.tmux.clone() else {
            return Ok(());
        };
        let mut stale = Vec::new();
        for activation_id in activation_ids {
            let Some(pane) = tmux.panes.get(activation_id) else {
                continue;
            };
            let metadata = TmuxActivationMetadata::new(
                &pane.session_id,
                &self.config.run_id,
                &pane.window_name,
                &pane.window_id,
                activation_id,
                &pane.pane_id,
            );
            match self
                .tmux_adapter
                .probe_exact_pane_presence(&metadata)
                .map_err(DriverFailure::from_tmux)?
            {
                TmuxPanePresence::Present => {}
                TmuxPanePresence::Absent => stale.push(activation_id.clone()),
            }
        }
        for activation_id in stale {
            self.recover_activation_pane_cleanup(&activation_id, "stale_replay")?;
        }
        Ok(())
    }

    pub(super) fn reconcile_replayed_tmux_panes(&mut self) -> Result<(), DriverFailure> {
        let activation_ids = self
            .tmux
            .as_ref()
            .map(|tmux| tmux.panes.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        self.reconcile_stale_tmux_panes(&activation_ids)
    }

    fn append_tmux_binding(&mut self, tmux: &DriverTmuxState) -> Result<(), DriverFailure> {
        self.append_driver_event(
            "tmux_bound",
            json!({
                "session_id": tmux.session_id,
                "window_name": tmux.window_name,
                "window_id": tmux.window_id,
                "agent_command": tmux.agent_command,
                "actuation": tmux.actuation
            }),
        )
    }

    fn publish_tmux_pane(
        &mut self,
        activation_id: &str,
        pane: StoredPane,
        operation_id: &str,
    ) -> Result<(), DriverFailure> {
        let cleanup = pane.to_json(activation_id);
        if let Err(err) = self.append_driver_event(
            "tmux_pane_allocated",
            json!({
                "activation_id": activation_id,
                "operation_id": operation_id,
                "pane": pane
            }),
        ) {
            return Err(err.with_tmux_cleanup(vec![cleanup]));
        }
        if let Some(tmux) = self.tmux.as_mut() {
            tmux.panes.insert(activation_id.to_string(), pane.clone());
        }
        self.allocation_generations
            .insert(activation_id.to_string(), pane.allocation_generation);
        self.pending_tmux_allocations.remove(activation_id);
        Ok(())
    }

    fn next_allocation_generation(&self, activation_id: &str) -> u64 {
        self.allocation_generations
            .get(activation_id)
            .map_or(0, |generation| generation.saturating_add(1))
    }

    fn allocate_missing_tmux_panes(&mut self) -> Result<Vec<Value>, DriverFailure> {
        let running = self
            .driver
            .runtime()
            .state()
            .activations
            .values()
            .filter(|activation| activation.run_id == self.config.run_id)
            .filter(|activation| activation.status == runtime::ActivationStatus::Running)
            .map(|activation| activation.activation_id.clone())
            .collect::<Vec<_>>();
        let config = self.tmux.as_ref().map(DriverTmuxState::config).or_else(|| {
            running
                .iter()
                .find_map(|activation_id| self.pending_tmux_allocations.get(activation_id))
                .map(TmuxPaneAllocationIntent::config)
        });
        let Some(config) = config else {
            return Ok(Vec::new());
        };
        let missing = running
            .into_iter()
            .filter(|activation_id| {
                self.tmux
                    .as_ref()
                    .is_none_or(|tmux| !tmux.panes.contains_key(activation_id))
            })
            .collect::<Vec<_>>();
        self.ensure_tmux_for_activations(config, &missing)
    }

    pub(super) fn reconcile_post_commit(
        &mut self,
    ) -> Result<(Vec<Value>, DriverActuation), DriverFailure> {
        self.reconcile_post_commit_with_tmux_config(None, &[])
    }

    fn reconcile_post_commit_with_tmux_config(
        &mut self,
        config: Option<DriverTmuxConfig>,
        activation_ids: &[String],
    ) -> Result<(Vec<Value>, DriverActuation), DriverFailure> {
        if !runtime::scheduling_enabled(
            self.driver.runtime().state(),
            &self.config.run_id,
            runtime::SchedulingIntent::Explicit,
        ) {
            return Ok((Vec::new(), DriverActuation::default()));
        }
        let result = match config {
            Some(config) => match self.ensure_tmux_for_activations(config, activation_ids) {
                Ok(panes) => {
                    let pending = self.pending_tmux_actuation_ids();
                    self.actuate_activations(&pending)
                        .map(|actuation| (panes, actuation))
                }
                Err(err) => Err(err),
            },
            None => self.allocate_and_actuate_tmux(),
        };
        match result {
            Ok((panes, actuation)) => {
                if actuation.requires_pause() {
                    self.pause_for_reconciliation()?;
                }
                Ok((panes, actuation))
            }
            Err(err) => {
                self.pause_for_reconciliation()?;
                Err(err)
            }
        }
    }

    pub(super) fn pause_for_reconciliation(&mut self) -> Result<(), DriverFailure> {
        if !runtime::scheduling_enabled(
            self.driver.runtime().state(),
            &self.config.run_id,
            runtime::SchedulingIntent::Explicit,
        ) {
            return Ok(());
        }
        let mut next_driver = self.driver.clone();
        next_driver
            .runtime_mut()
            .set_run_status_with_reason(
                &self.config.run_id,
                runtime::RunStatus::Paused,
                Some("effect_reconciliation_required"),
            )
            .map_err(DriverFailure::from_runtime)?;
        self.commit_runtime(next_driver)
    }

    fn allocate_and_actuate_tmux(
        &mut self,
    ) -> Result<(Vec<Value>, DriverActuation), DriverFailure> {
        let tmux_allocations = self.allocate_missing_tmux_panes()?;
        let activation_ids = self.pending_tmux_actuation_ids();
        let actuation = self.actuate_activations(&activation_ids)?;
        Ok((tmux_allocations, actuation))
    }

    fn pending_tmux_actuation_ids(&self) -> Vec<String> {
        let Some(tmux) = self.tmux.as_ref() else {
            return Vec::new();
        };
        self.driver
            .runtime()
            .state()
            .activations
            .values()
            .filter(|activation| activation.run_id == self.config.run_id)
            .filter(|activation| activation.status == runtime::ActivationStatus::Running)
            .filter(|activation| tmux.panes.contains_key(&activation.activation_id))
            .filter(|activation| {
                tmux.panes
                    .get(&activation.activation_id)
                    .is_some_and(|pane| {
                        !self.settled_actuation_activations.contains(&(
                            activation.activation_id.clone(),
                            pane.allocation_generation,
                        ))
                    })
            })
            .map(|activation| activation.activation_id.clone())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DriverTmuxState {
    pub(super) session_id: String,
    pub(super) window_name: String,
    pub(super) window_id: String,
    pub(super) agent_command: String,
    #[serde(default)]
    pub(super) actuation: DriverTmuxActuationConfig,
    pub(super) panes: BTreeMap<String, StoredPane>,
}

impl DriverTmuxState {
    fn config(&self) -> DriverTmuxConfig {
        DriverTmuxConfig {
            session: self.session_id.clone(),
            window: self.window_name.clone(),
            agent_command: self.agent_command.clone(),
            actuation: self.actuation.clone(),
            existing_window_id: Some(self.window_id.clone()),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct StoredPane {
    pub(super) pane_id: String,
    pub(super) session_id: String,
    pub(super) window_id: String,
    pub(super) window_name: String,
    #[serde(default)]
    pub(super) allocation_generation: u64,
}

impl StoredPane {
    fn from_tmux_pane(window: &TmuxWindow, pane: &TmuxPane, allocation_generation: u64) -> Self {
        Self {
            pane_id: pane.id().to_string(),
            session_id: window.session_id().to_string(),
            window_id: window.id().to_string(),
            window_name: window.name().to_string(),
            allocation_generation,
        }
    }

    pub(super) fn to_json(&self, activation_id: &str) -> Value {
        json!({
            "activation_id": activation_id,
            "pane_id": self.pane_id,
            "session_id": self.session_id,
            "window_id": self.window_id,
            "window_name": self.window_name,
            "allocation_generation": self.allocation_generation
        })
    }
}

fn tmux_allocation_operation_id(
    run_id: &str,
    activation_id: &str,
    allocation_generation: u64,
) -> String {
    let mut hash = Sha256::new();
    for value in [
        "tmux-pane-allocation",
        run_id,
        activation_id,
        &allocation_generation.to_string(),
    ] {
        hash.update(value.len().to_be_bytes());
        hash.update(value.as_bytes());
    }
    format!("alloc_{}", hex_digest(&hash.finalize()))
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[derive(Debug, Clone)]
struct DriverTmuxConfig {
    session: String,
    window: String,
    agent_command: String,
    actuation: DriverTmuxActuationConfig,
    existing_window_id: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DriverTmuxActuationConfig {
    #[serde(default = "default_prompt_submit_key_count")]
    pub(crate) prompt_submit_key_count: usize,
    #[serde(default)]
    pub(crate) agent_ready_pattern: Option<String>,
    #[serde(default = "default_agent_ready_timeout_ms")]
    pub(crate) agent_ready_timeout_ms: u64,
}

impl Default for DriverTmuxActuationConfig {
    fn default() -> Self {
        Self {
            prompt_submit_key_count: default_prompt_submit_key_count(),
            agent_ready_pattern: None,
            agent_ready_timeout_ms: default_agent_ready_timeout_ms(),
        }
    }
}

pub(crate) fn parse_tmux_actuation_config(
    object: &serde_json::Map<String, Value>,
) -> Result<DriverTmuxActuationConfig, String> {
    let mut config = DriverTmuxActuationConfig::default();
    if let Some(value) = aliased_field(object, "prompt_submit_key_count", "promptSubmitKeyCount") {
        let count = value.as_u64().ok_or_else(|| {
            "tmux.prompt_submit_key_count must be an unsigned integer".to_string()
        })?;
        if !(1..=4).contains(&count) {
            return Err("tmux.prompt_submit_key_count must be between 1 and 4".to_string());
        }
        config.prompt_submit_key_count = count as usize;
    }
    if let Some(value) = aliased_field(object, "agent_ready_pattern", "agentReadyPattern") {
        let pattern = value
            .as_str()
            .ok_or_else(|| "tmux.agent_ready_pattern must be a string".to_string())?
            .trim();
        if pattern.is_empty() {
            return Err("tmux.agent_ready_pattern must be non-empty".to_string());
        }
        config.agent_ready_pattern = Some(pattern.to_string());
    }
    if let Some(value) = aliased_field(object, "agent_ready_timeout_ms", "agentReadyTimeoutMs") {
        let timeout_ms = value
            .as_u64()
            .ok_or_else(|| "tmux.agent_ready_timeout_ms must be an unsigned integer".to_string())?;
        if !(100..=300_000).contains(&timeout_ms) {
            return Err("tmux.agent_ready_timeout_ms must be between 100 and 300000".to_string());
        }
        config.agent_ready_timeout_ms = timeout_ms;
    }
    Ok(config)
}

fn aliased_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    canonical: &str,
    alias: &str,
) -> Option<&'a Value> {
    object.get(canonical).or_else(|| object.get(alias))
}

const fn default_prompt_submit_key_count() -> usize {
    1
}

const fn default_agent_ready_timeout_ms() -> u64 {
    30_000
}

fn tmux_config_from_request(request: &Value) -> Result<Option<DriverTmuxConfig>, DriverFailure> {
    let Some(tmux) = request.get("tmux") else {
        return Ok(None);
    };
    let object = tmux
        .as_object()
        .ok_or_else(|| DriverFailure::new("malformed_request", "tmux must be an object"))?;
    if object.get("enabled").and_then(Value::as_bool) != Some(true) {
        return Ok(None);
    }
    let session = string_field(object, "session")?.trim().to_string();
    let window = string_field(object, "window")?.trim().to_string();
    let agent_command = object
        .get("agent_command")
        .or_else(|| object.get("agentCommand"))
        .and_then(Value::as_str)
        .ok_or_else(|| DriverFailure::new("malformed_request", "tmux.agent_command is required"))?
        .trim()
        .to_string();
    if session.is_empty() || window.is_empty() || agent_command.is_empty() {
        return Err(DriverFailure::new(
            "malformed_request",
            "tmux session, window, and agent command must be non-empty",
        ));
    }
    Ok(Some(DriverTmuxConfig {
        session,
        window,
        agent_command,
        actuation: parse_tmux_actuation_config(object)
            .map_err(|message| DriverFailure::new("malformed_request", message))?,
        existing_window_id: object
            .get("window_id")
            .or_else(|| object.get("windowId"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }))
}
