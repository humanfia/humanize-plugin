use std::collections::BTreeMap;

use crate::flow;

use super::{
    ActivationStatus, EventPayload, NodeSpec, RouteTrigger, RunMode, RunStatus, Runtime,
    RuntimeError, RuntimeState, SchedulingIntent, StopDecision, StopDecisionKind, StopObservation,
    scheduling_enabled,
};

pub type RunCompletionMode = RunMode;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct LoopBudget {
    pub tick_limit: u64,
    pub action_limit: u64,
}

impl Default for LoopBudget {
    fn default() -> Self {
        Self {
            tick_limit: 1,
            action_limit: 32,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TickBudget {
    pub stop_validation_attempt_limit: u32,
    pub stop_validations_per_tick: usize,
}

impl Default for TickBudget {
    fn default() -> Self {
        Self {
            stop_validation_attempt_limit: 3,
            stop_validations_per_tick: 32,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ControlCommand {
    StartRun {
        run_id: String,
        nodes: Vec<NodeSpec>,
    },
    PauseRun {
        run_id: String,
    },
    ResumeRun {
        run_id: String,
    },
    StopRun {
        run_id: String,
    },
    CompleteRun {
        run_id: String,
    },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct StopObservationInput {
    pub run_id: String,
    pub activation_id: String,
    pub observation: StopObservation,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DriverTickInput {
    pub controls: Vec<ControlCommand>,
    pub stop_observations: Vec<StopObservationInput>,
    pub route_locks: Vec<flow::FlowLock>,
    pub tick_budget: TickBudget,
    pub loop_budget: LoopBudget,
    pub completion_mode: RunCompletionMode,
    pub activation_limit: Option<u64>,
}

impl Default for DriverTickInput {
    fn default() -> Self {
        Self {
            controls: Vec::new(),
            stop_observations: Vec::new(),
            route_locks: Vec::new(),
            tick_budget: TickBudget::default(),
            loop_budget: LoopBudget::default(),
            completion_mode: RunCompletionMode::Finite,
            activation_limit: None,
        }
    }
}

impl DriverTickInput {
    pub fn with_control(mut self, control: ControlCommand) -> Self {
        self.controls.push(control);
        self
    }

    pub fn with_stop_observation(
        mut self,
        run_id: impl Into<String>,
        activation_id: impl Into<String>,
        observation: StopObservation,
    ) -> Self {
        self.stop_observations.push(StopObservationInput {
            run_id: run_id.into(),
            activation_id: activation_id.into(),
            observation,
        });
        self
    }

    pub fn with_route_lock(mut self, lock: flow::FlowLock) -> Self {
        self.route_locks.push(lock);
        self
    }

    pub fn with_budget(mut self, budget: TickBudget) -> Self {
        self.tick_budget = budget;
        self
    }

    pub fn with_loop_budget(mut self, budget: LoopBudget) -> Self {
        self.loop_budget = budget;
        self
    }

    pub fn with_completion_mode(mut self, completion_mode: RunCompletionMode) -> Self {
        self.completion_mode = completion_mode;
        self
    }

    pub fn with_run_mode(self, run_mode: RunMode) -> Self {
        self.with_completion_mode(run_mode)
    }

    pub fn with_activation_limit(mut self, activation_limit: u64) -> Self {
        self.activation_limit = Some(activation_limit);
        self
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DriverRender {
    pub run_statuses: BTreeMap<String, RunStatus>,
    pub activation_statuses: BTreeMap<String, ActivationStatus>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DriverTickReport {
    pub pipeline: Vec<&'static str>,
    pub control_errors: Vec<RuntimeError>,
    pub stop_decisions: Vec<StopDecision>,
    pub route_decisions: Vec<RouteDecision>,
    pub render: DriverRender,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RouteDecision {
    pub run_id: String,
    pub flow_lock_id: String,
    pub route_index: usize,
    pub route_id: String,
    pub predicate: String,
    pub for_each: Option<String>,
    pub source_artifact: Option<RouteSourceArtifact>,
    pub trigger: RouteTrigger,
    pub planned_activation_ids: Vec<String>,
    pub applied_activation_ids: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RouteSourceArtifact {
    pub key: String,
    pub artifact_id: Option<String>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct DriverState {
    runtime: Runtime,
    ticks: u64,
}

impl DriverState {
    pub fn from_runtime(runtime: Runtime) -> Self {
        Self { runtime, ticks: 0 }
    }

    pub fn record_participant_exit(
        &mut self,
        run_id: &str,
        activation_id: &str,
        allocation_generation: u64,
        exit_status: i32,
    ) -> Result<bool, RuntimeError> {
        if !self
            .runtime
            .state
            .activations
            .contains_key(&(run_id.to_string(), activation_id.to_string()))
        {
            return Err(RuntimeError::ActivationNotFoundInRun {
                run_id: run_id.to_string(),
                activation_id: activation_id.to_string(),
            });
        }
        if let Some(recorded) =
            self.runtime
                .events()
                .iter()
                .find_map(|event| match &event.payload {
                    EventPayload::ParticipantExited {
                        run_id: recorded_run,
                        activation_id: recorded_activation,
                        allocation_generation: recorded_generation,
                        exit_status,
                    } if recorded_run == run_id
                        && recorded_activation == activation_id
                        && *recorded_generation == allocation_generation =>
                    {
                        Some(*exit_status)
                    }
                    _ => None,
                })
        {
            if recorded == exit_status {
                return Ok(false);
            }
            return Err(RuntimeError::ParticipantExitConflict {
                run_id: run_id.to_string(),
                activation_id: activation_id.to_string(),
                allocation_generation,
            });
        }
        self.runtime.append(EventPayload::ParticipantExited {
            run_id: run_id.to_string(),
            activation_id: activation_id.to_string(),
            allocation_generation,
            exit_status,
        });
        Ok(true)
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    pub fn runtime_mut(&mut self) -> &mut Runtime {
        &mut self.runtime
    }

    pub fn tick(&mut self, input: DriverTickInput) -> DriverTickReport {
        let pipeline = vec![
            "Replay",
            "Handle Control",
            "Observe",
            "Validate",
            "Route",
            "Actuate",
            "Complete",
            "Render",
        ];
        self.ticks = self.ticks.saturating_add(1);

        self.replay();
        let control_errors = self.handle_control(
            &input.controls,
            input.completion_mode,
            input.activation_limit,
        );

        let mut stop_decisions = Vec::new();
        let mut route_decisions = Vec::new();
        let mut pending_stop_observations = input.stop_observations.clone();
        let mut action_budget = input.loop_budget.action_limit;
        for _ in 0..input.loop_budget.tick_limit {
            let observed = self.observe(&mut pending_stop_observations);
            let mut progressed = !observed.is_empty();

            let decisions = self.validate(&observed, input.tick_budget);
            progressed |= !decisions.is_empty();
            stop_decisions.extend(decisions);

            progressed |= self.route(&input.route_locks, &mut action_budget, &mut route_decisions);
            progressed |= self.actuate(&mut action_budget);
            progressed |= self.complete(&input.route_locks, &mut action_budget);

            if !progressed || action_budget == 0 {
                break;
            }
        }

        let render = self.render();

        DriverTickReport {
            pipeline,
            control_errors,
            stop_decisions,
            route_decisions,
            render,
        }
    }

    fn replay(&mut self) {
        let events = self.runtime.store.replay().to_vec();
        self.runtime.state = RuntimeState::from_events(&events);
    }

    fn handle_control(
        &mut self,
        controls: &[ControlCommand],
        default_completion_mode: RunCompletionMode,
        requested_activation_limit: Option<u64>,
    ) -> Vec<RuntimeError> {
        let mut errors = Vec::new();
        for control in controls {
            let result = match control {
                ControlCommand::StartRun { run_id, nodes } => {
                    let activation_limit = requested_activation_limit.unwrap_or(u64::MAX);
                    if self.runtime.has_run(run_id) {
                        let actual_mode = self.runtime.state.run_mode(run_id).unwrap_or_default();
                        let actual_limit = self
                            .runtime
                            .state
                            .initial_activation_limit(run_id)
                            .unwrap_or(u64::MAX);
                        if actual_mode != default_completion_mode
                            || actual_limit != activation_limit
                        {
                            Err(RuntimeError::RunConfigurationConflict {
                                run_id: run_id.clone(),
                                expected_mode: actual_mode,
                                actual_mode: default_completion_mode,
                                expected_activation_limit: actual_limit,
                                actual_activation_limit: activation_limit,
                            })
                        } else {
                            self.runtime.set_run_status(run_id, RunStatus::Running)
                        }
                    } else {
                        self.runtime
                            .start_run_with_options(
                                run_id.clone(),
                                nodes.clone(),
                                default_completion_mode,
                                activation_limit,
                            )
                            .and_then(|_| self.runtime.set_run_status(run_id, RunStatus::Running))
                    }
                }
                ControlCommand::PauseRun { run_id } => self.pause_run(run_id),
                ControlCommand::ResumeRun { run_id } => {
                    self.resume_run(run_id, requested_activation_limit)
                }
                ControlCommand::StopRun { run_id } => self.request_stop(run_id),
                ControlCommand::CompleteRun { run_id } => self.complete_run(run_id),
            };
            if let Err(error) = result {
                errors.push(error);
            }
        }
        errors
    }

    fn request_stop(&mut self, run_id: &str) -> Result<(), RuntimeError> {
        let status = self.run_status_for_control(run_id)?;
        if is_terminal_run_status(status) {
            return Ok(());
        }
        self.runtime.set_run_status(run_id, RunStatus::Stopping)
    }

    fn resume_run(
        &mut self,
        run_id: &str,
        requested_activation_limit: Option<u64>,
    ) -> Result<(), RuntimeError> {
        let status = self.run_status_for_control(run_id)?;
        if !matches!(status, RunStatus::Paused | RunStatus::Quiescent) {
            return Err(RuntimeError::InvalidRunStatusTransition {
                run_id: run_id.to_owned(),
                action: "resume".to_string(),
                status,
            });
        }
        let current_limit = self.runtime.state.activation_limit(run_id).ok_or_else(|| {
            RuntimeError::RunNotFound {
                run_id: run_id.to_owned(),
            }
        })?;
        if self.runtime.state.run_status_reason(run_id) == Some("activation_limit_exhausted")
            && requested_activation_limit.is_none_or(|limit| limit <= current_limit)
        {
            return Err(RuntimeError::ActivationLimitIncreaseRequired {
                run_id: run_id.to_owned(),
                current: current_limit,
            });
        }
        if let Some(limit) = requested_activation_limit {
            self.runtime.raise_activation_limit(run_id, limit)?;
        }
        self.runtime.set_run_status(run_id, RunStatus::Running)
    }

    fn pause_run(&mut self, run_id: &str) -> Result<(), RuntimeError> {
        let status = self.run_status_for_control(run_id)?;
        match status {
            RunStatus::Running | RunStatus::Quiescent => {
                self.runtime
                    .set_run_status_with_reason(run_id, RunStatus::Paused, None)
            }
            RunStatus::Paused => Ok(()),
            _ => Err(RuntimeError::InvalidRunStatusTransition {
                run_id: run_id.to_owned(),
                action: "pause".to_string(),
                status,
            }),
        }
    }

    fn complete_run(&mut self, run_id: &str) -> Result<(), RuntimeError> {
        let status = self.run_status_for_control(run_id)?;
        if status != RunStatus::Quiescent {
            return Err(RuntimeError::RunNotQuiescent {
                run_id: run_id.to_owned(),
                status,
            });
        }
        let mode = self.runtime.state.run_mode(run_id).unwrap_or_default();
        if mode != RunMode::Manual {
            return Err(RuntimeError::RunModeDoesNotAllowControl {
                run_id: run_id.to_owned(),
                action: "complete".to_string(),
                mode,
            });
        }
        self.runtime.set_run_status(run_id, RunStatus::Completed)
    }

    fn run_status_for_control(&self, run_id: &str) -> Result<RunStatus, RuntimeError> {
        self.runtime
            .state
            .run_status(run_id)
            .ok_or_else(|| RuntimeError::RunNotFound {
                run_id: run_id.to_owned(),
            })
    }

    fn observe(
        &mut self,
        pending_observations: &mut Vec<StopObservationInput>,
    ) -> Vec<StopObservationInput> {
        let mut observed_now = Vec::new();
        let mut still_pending = Vec::new();

        for observed in pending_observations.drain(..) {
            let Some(status) = self
                .runtime
                .state
                .activations
                .get(&(observed.run_id.clone(), observed.activation_id.clone()))
                .map(|activation| activation.status)
            else {
                still_pending.push(observed);
                continue;
            };
            if matches!(
                status,
                ActivationStatus::Completed
                    | ActivationStatus::Failed
                    | ActivationStatus::Cancelled
            ) {
                continue;
            }

            self.runtime.append(EventPayload::StopObserved {
                run_id: observed.run_id.clone(),
                activation_id: observed.activation_id.clone(),
                observation: observed.observation.clone(),
            });
            self.set_activation_status(
                &observed.run_id,
                &observed.activation_id,
                ActivationStatus::WaitingForStop,
            );
            observed_now.push(observed);
        }

        *pending_observations = still_pending;
        observed_now
    }

    fn validate(
        &mut self,
        observations: &[StopObservationInput],
        tick_budget: TickBudget,
    ) -> Vec<StopDecision> {
        let mut decisions = Vec::new();
        let mut validations = 0_usize;
        let attempt_limit = tick_budget.stop_validation_attempt_limit.max(1);

        for observed in observations {
            let prior_attempt = self
                .runtime
                .state
                .stop_validation_attempts
                .get(&(observed.run_id.clone(), observed.activation_id.clone()))
                .copied()
                .unwrap_or(0);

            if validations >= tick_budget.stop_validations_per_tick {
                let decision =
                    StopDecision::yield_now(prior_attempt, "stop validation budget exhausted");
                self.record_stop_decision(observed, decision.clone());
                decisions.push(decision);
                continue;
            }

            let attempt = prior_attempt.saturating_add(1);
            validations += 1;
            self.set_activation_status(
                &observed.run_id,
                &observed.activation_id,
                ActivationStatus::ValidatingStop,
            );
            let (missing_artifacts, missing_effects) =
                self.missing_stop_requirements(&observed.run_id, &observed.activation_id);
            let decision = if missing_artifacts.is_empty() && missing_effects.is_empty() {
                StopDecision::allow(attempt)
            } else if attempt >= attempt_limit {
                StopDecision::block(attempt, missing_artifacts, missing_effects)
            } else {
                StopDecision::deny_until_limit(attempt, missing_artifacts, missing_effects)
            };
            self.record_stop_decision(observed, decision.clone());
            match decision.kind {
                StopDecisionKind::Allow => self.set_activation_status(
                    &observed.run_id,
                    &observed.activation_id,
                    ActivationStatus::Completed,
                ),
                StopDecisionKind::Block => self.set_activation_status(
                    &observed.run_id,
                    &observed.activation_id,
                    ActivationStatus::Blocked,
                ),
                StopDecisionKind::Deny | StopDecisionKind::Yield => self.set_activation_status(
                    &observed.run_id,
                    &observed.activation_id,
                    ActivationStatus::WaitingForStop,
                ),
            };
            decisions.push(decision);
        }

        decisions
    }

    fn record_stop_decision(&mut self, observed: &StopObservationInput, decision: StopDecision) {
        self.runtime.append(EventPayload::StopDecision {
            run_id: observed.run_id.clone(),
            activation_id: observed.activation_id.clone(),
            decision,
        });
    }

    fn route(
        &mut self,
        route_locks: &[flow::FlowLock],
        action_budget: &mut u64,
        route_decisions: &mut Vec<RouteDecision>,
    ) -> bool {
        if route_locks.is_empty() || *action_budget == 0 {
            return false;
        }

        let mut progressed = false;
        let run_ids = self.runtime.state.runs.iter().cloned().collect::<Vec<_>>();
        for run_id in run_ids {
            if *action_budget == 0 {
                break;
            }
            let status = self.runtime.state.run_status(&run_id);
            if !scheduling_enabled(
                &self.runtime.state,
                &run_id,
                SchedulingIntent::FactTriggeredRoute,
            ) {
                continue;
            }

            let Some(lock_id) = self.runtime.state.flow_lock_id_by_run.get(&run_id) else {
                continue;
            };
            let Some(lock) = route_locks.iter().find(|lock| lock.id() == lock_id) else {
                continue;
            };
            let Ok(previews) = super::preview_flow_routes(&self.runtime.state, &run_id, lock)
            else {
                continue;
            };

            for preview in previews {
                if *action_budget == 0 {
                    break;
                }
                if preview.reason.as_deref() == Some("activation_limit_exhausted") {
                    progressed |= self.set_run_status_with_reason(
                        &run_id,
                        RunStatus::Paused,
                        Some("activation_limit_exhausted"),
                    );
                    break;
                }
                if !preview.matched {
                    continue;
                }
                let planned_count = preview.planned_activations.len() as u64;
                if planned_count == 0 {
                    continue;
                }

                let Some(route) = lock.draft().routes.get(preview.route_index) else {
                    continue;
                };
                let Some(trigger) = preview.trigger.clone() else {
                    continue;
                };
                let planned_activation_ids = preview
                    .planned_activations
                    .iter()
                    .map(|activation| activation.activation_id.clone())
                    .collect::<Vec<_>>();
                let source_artifact = route_source_artifact(&self.runtime.state, &run_id, route);
                let node = match route.for_each.as_ref() {
                    Some(for_each) => {
                        node_spec_for_route(lock, route).with_for_each(for_each.clone())
                    }
                    None => node_spec_for_route(lock, route),
                };
                let activated = match self.runtime.apply_route_plan(
                    &run_id,
                    &node,
                    &trigger,
                    &preview.planned_activations,
                ) {
                    Ok(activated) => activated,
                    Err(RuntimeError::ActivationLimitExceeded { .. }) => {
                        progressed |= self.set_run_status_with_reason(
                            &run_id,
                            RunStatus::Paused,
                            Some("activation_limit_exhausted"),
                        );
                        break;
                    }
                    Err(_) => continue,
                };

                if activated.is_empty() {
                    continue;
                }
                route_decisions.push(RouteDecision {
                    run_id: run_id.clone(),
                    flow_lock_id: lock.id().to_string(),
                    route_index: preview.route_index,
                    route_id: preview.route_id,
                    predicate: route.predicate.to_string(),
                    for_each: route.for_each.as_ref().map(ToString::to_string),
                    source_artifact,
                    trigger,
                    planned_activation_ids,
                    applied_activation_ids: activated.clone(),
                });
                if status == Some(RunStatus::Quiescent) {
                    self.set_run_status(&run_id, RunStatus::Running);
                }
                *action_budget = (*action_budget).saturating_sub(1);
                progressed = true;
            }
        }

        progressed
    }

    fn actuate(&mut self, action_budget: &mut u64) -> bool {
        let activations = self
            .runtime
            .state
            .activations
            .values()
            .filter(|activation| {
                activation.status == ActivationStatus::Pending
                    && scheduling_enabled(
                        &self.runtime.state,
                        &activation.run_id,
                        SchedulingIntent::Explicit,
                    )
            })
            .map(|activation| (activation.run_id.clone(), activation.activation_id.clone()))
            .collect::<Vec<_>>();

        let mut progressed = false;
        for (run_id, activation_id) in activations {
            if *action_budget == 0 {
                break;
            }
            self.set_activation_status(&run_id, &activation_id, ActivationStatus::Starting);
            self.set_activation_status(&run_id, &activation_id, ActivationStatus::Running);
            *action_budget = (*action_budget).saturating_sub(1);
            progressed = true;
        }
        progressed
    }

    fn complete(&mut self, route_locks: &[flow::FlowLock], action_budget: &mut u64) -> bool {
        let run_ids = self.runtime.state.runs.iter().cloned().collect::<Vec<_>>();
        let mut progressed = false;
        for run_id in run_ids {
            let status = self.runtime.state.run_status(&run_id);
            if status
                .is_some_and(|status| is_terminal_run_status(status) || status == RunStatus::Paused)
            {
                continue;
            }
            if status == Some(RunStatus::Stopping) {
                progressed |= self.stop_run(&run_id, action_budget);
                continue;
            }

            let activations = self
                .runtime
                .state
                .activations
                .values()
                .filter(|activation| activation.run_id == run_id)
                .map(|activation| activation.status)
                .collect::<Vec<_>>();
            if activations.contains(&ActivationStatus::Blocked) {
                if *action_budget == 0 {
                    break;
                }
                progressed |= self.set_run_status(&run_id, RunStatus::Blocked);
                *action_budget = (*action_budget).saturating_sub(1);
                continue;
            }
            if activations.iter().all(|status| {
                matches!(
                    status,
                    ActivationStatus::Completed
                        | ActivationStatus::Failed
                        | ActivationStatus::Cancelled
                )
            }) {
                if self.has_pending_route(&run_id, route_locks) {
                    continue;
                }
                let completion_mode = self.runtime.state.run_mode(&run_id).unwrap_or_default();
                match completion_mode {
                    RunCompletionMode::Finite => {
                        if *action_budget == 0 {
                            break;
                        }
                        progressed |= self.set_run_status(&run_id, RunStatus::Completed);
                        *action_budget = (*action_budget).saturating_sub(1);
                    }
                    RunCompletionMode::Continuous | RunCompletionMode::Manual => {
                        if *action_budget == 0 {
                            break;
                        }
                        progressed |= self.set_run_status(&run_id, RunStatus::Quiescent);
                        *action_budget = (*action_budget).saturating_sub(1);
                    }
                }
            }
        }
        progressed
    }

    fn has_pending_route(&self, run_id: &str, route_locks: &[flow::FlowLock]) -> bool {
        let Some(lock_id) = self.runtime.state.flow_lock_id_by_run.get(run_id) else {
            return false;
        };
        let Some(lock) = route_locks.iter().find(|lock| lock.id() == lock_id) else {
            return false;
        };
        super::preview_flow_routes(&self.runtime.state, run_id, lock).is_ok_and(|previews| {
            previews.iter().any(|preview| {
                preview.matched || preview.reason.as_deref() == Some("activation_limit_exhausted")
            })
        })
    }

    fn render(&self) -> DriverRender {
        let activation_statuses = self
            .runtime
            .state
            .activations
            .values()
            .map(|activation| {
                (
                    format!("{}/{}", activation.run_id, activation.activation_id),
                    activation.status,
                )
            })
            .collect();

        DriverRender {
            run_statuses: self.runtime.state.run_statuses.clone(),
            activation_statuses,
        }
    }

    fn stop_run(&mut self, run_id: &str, action_budget: &mut u64) -> bool {
        let activations = self
            .runtime
            .state
            .activations
            .values()
            .filter(|activation| activation.run_id == run_id)
            .filter(|activation| {
                !matches!(
                    activation.status,
                    ActivationStatus::Completed
                        | ActivationStatus::Failed
                        | ActivationStatus::Cancelled
                )
            })
            .map(|activation| activation.activation_id.clone())
            .collect::<Vec<_>>();

        let mut progressed = false;
        for activation_id in activations {
            if *action_budget == 0 {
                return progressed;
            }
            progressed |=
                self.set_activation_status(run_id, &activation_id, ActivationStatus::Cancelled);
            *action_budget = (*action_budget).saturating_sub(1);
        }
        if *action_budget == 0 {
            return progressed;
        }
        progressed |= self.set_run_status(run_id, RunStatus::Stopped);
        *action_budget = (*action_budget).saturating_sub(1);
        progressed
    }

    fn set_run_status(&mut self, run_id: &str, status: RunStatus) -> bool {
        self.set_run_status_with_reason(run_id, status, None)
    }

    fn set_run_status_with_reason(
        &mut self,
        run_id: &str,
        status: RunStatus,
        reason: Option<&str>,
    ) -> bool {
        if self.runtime.state.run_status(run_id) == Some(status)
            && self.runtime.state.run_status_reason(run_id) == reason
        {
            return false;
        }
        self.runtime.append(EventPayload::RunStatusChanged {
            run_id: run_id.to_owned(),
            status,
            reason: reason.map(str::to_owned),
        });
        true
    }

    fn set_activation_status(
        &mut self,
        run_id: &str,
        activation_id: &str,
        status: ActivationStatus,
    ) -> bool {
        let key = (run_id.to_owned(), activation_id.to_owned());
        if self
            .runtime
            .state
            .activations
            .get(&key)
            .map(|activation| activation.status)
            == Some(status)
        {
            return false;
        }
        if self.runtime.state.activations.contains_key(&key) {
            self.runtime.append(EventPayload::ActivationStatusChanged {
                run_id: run_id.to_owned(),
                activation_id: activation_id.to_owned(),
                status,
            });
            return true;
        }
        false
    }

    fn missing_stop_requirements(
        &self,
        run_id: &str,
        activation_id: &str,
    ) -> (Vec<String>, Vec<String>) {
        let Some(activation) = self
            .runtime
            .state
            .activations
            .get(&(run_id.to_owned(), activation_id.to_owned()))
        else {
            return (vec!["activation".into()], Vec::new());
        };

        let missing_artifacts = activation
            .stop_contract
            .required_artifacts()
            .iter()
            .filter(|artifact_key| !activation.context.contains_key(*artifact_key))
            .cloned()
            .collect::<Vec<_>>();
        let missing_effects = activation
            .stop_contract
            .required_effects()
            .iter()
            .filter(|effect_key| {
                !self.runtime.state.effects.contains_key(&(
                    run_id.to_owned(),
                    activation_id.to_owned(),
                    (*effect_key).clone(),
                ))
            })
            .cloned()
            .collect::<Vec<_>>();

        (missing_artifacts, missing_effects)
    }
}

fn is_terminal_run_status(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Completed | RunStatus::Failed | RunStatus::Stopped
    )
}

fn route_source_artifact(
    state: &RuntimeState,
    run_id: &str,
    route: &flow::FlowRoute,
) -> Option<RouteSourceArtifact> {
    let key = if let Some(for_each) = &route.for_each {
        for_each.key()
    } else if let flow::FactRef::Artifact { key } = route.predicate.fact_ref() {
        key
    } else {
        return None;
    };
    let artifact_id = state
        .latest_artifact_by_slot_index
        .get(&(run_id.to_string(), key.as_str().to_string()))
        .cloned();
    Some(RouteSourceArtifact {
        key: key.to_string(),
        artifact_id,
    })
}

fn node_spec_for_route(lock: &flow::FlowLock, route: &flow::FlowRoute) -> NodeSpec {
    flow::NodeContract::from_draft(lock.draft())
        .into_iter()
        .find(|contract| contract.node_id == route.activate)
        .map(|contract| {
            let required_artifacts = contract
                .artifact_requirements
                .iter()
                .filter(|artifact| artifact.required)
                .map(|artifact| artifact.id.clone())
                .collect::<Vec<_>>();
            let required_effects = contract
                .effect_requirements
                .iter()
                .filter(|effect| effect.required)
                .map(|effect| effect.id.clone())
                .collect::<Vec<_>>();
            NodeSpec::new(contract.node_id).with_stop_contract(super::StopContract::new(
                required_artifacts,
                required_effects,
            ))
        })
        .unwrap_or_else(|| NodeSpec::new(route.activate.clone()))
}
