use std::collections::BTreeMap;

use crate::flow;

use super::{
    ActivationStatus, EventPayload, NodeSpec, RunStatus, Runtime, RuntimeState, StopDecision,
    StopDecisionKind, StopObservation,
};

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum RunCompletionMode {
    #[default]
    Finite,
    Continuous,
    Manual,
}

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
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DriverRender {
    pub run_statuses: BTreeMap<String, RunStatus>,
    pub activation_statuses: BTreeMap<String, ActivationStatus>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DriverTickReport {
    pub pipeline: Vec<&'static str>,
    pub stop_decisions: Vec<StopDecision>,
    pub render: DriverRender,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct DriverState {
    runtime: Runtime,
    completion_modes: BTreeMap<String, RunCompletionMode>,
    ticks: u64,
}

impl DriverState {
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
        self.handle_control(&input.controls, input.completion_mode);

        let mut stop_decisions = Vec::new();
        let mut pending_stop_observations = input.stop_observations.clone();
        let mut action_budget = input.loop_budget.action_limit;
        for _ in 0..input.loop_budget.tick_limit {
            let observed = self.observe(&mut pending_stop_observations);
            let mut progressed = !observed.is_empty();

            let decisions = self.validate(&observed, input.tick_budget);
            progressed |= !decisions.is_empty();
            stop_decisions.extend(decisions);

            progressed |= self.route(&input.route_locks, &mut action_budget);
            progressed |= self.actuate(&mut action_budget);
            progressed |= self.complete(&mut action_budget);

            if !progressed || action_budget == 0 {
                break;
            }
        }

        let render = self.render();

        DriverTickReport {
            pipeline,
            stop_decisions,
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
    ) {
        for control in controls {
            match control {
                ControlCommand::StartRun { run_id, nodes } => {
                    self.completion_modes
                        .insert(run_id.clone(), default_completion_mode);
                    if !self.runtime.has_run(run_id)
                        && self
                            .runtime
                            .start_run(run_id.clone(), nodes.clone())
                            .is_err()
                    {
                        self.set_run_status(run_id, RunStatus::Failed);
                        continue;
                    }
                    self.set_run_status(run_id, RunStatus::Running);
                }
                ControlCommand::PauseRun { run_id } => {
                    self.set_run_status(run_id, RunStatus::Paused);
                }
                ControlCommand::ResumeRun { run_id } => {
                    self.set_run_status(run_id, RunStatus::Running);
                }
                ControlCommand::StopRun { run_id } => {
                    self.set_run_status(run_id, RunStatus::Stopping);
                }
                ControlCommand::CompleteRun { run_id } => {
                    self.set_run_status(run_id, RunStatus::Completed);
                }
            }
        }
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

    fn route(&mut self, route_locks: &[flow::FlowLock], action_budget: &mut u64) -> bool {
        if route_locks.is_empty() || *action_budget == 0 {
            return false;
        }

        let mut progressed = false;
        let run_ids = self.runtime.state.runs.iter().cloned().collect::<Vec<_>>();
        for run_id in run_ids {
            if *action_budget == 0 {
                break;
            }
            if self.runtime.state.run_status(&run_id) != Some(RunStatus::Running) {
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

            for preview in previews.into_iter().filter(|preview| preview.matched) {
                if *action_budget == 0 {
                    break;
                }
                let planned_count = preview.planned_activations.len() as u64;
                if planned_count == 0 {
                    continue;
                }
                if planned_count > *action_budget {
                    *action_budget = 0;
                    break;
                }

                let Some(route) = lock.draft().routes.get(preview.route_index) else {
                    continue;
                };
                let activated = match route.for_each.as_deref() {
                    Some(for_each) => {
                        let Some(artifact_key) = route_for_each_artifact_key(for_each) else {
                            continue;
                        };
                        let node = node_spec_for_route(lock, route).with_for_each(artifact_key);
                        self.runtime
                            .fanout_from_artifact(&run_id, &node, artifact_key)
                            .unwrap_or_default()
                    }
                    None => self
                        .runtime
                        .activate_node(&run_id, &node_spec_for_route(lock, route), None)
                        .map(|activation_id| vec![activation_id])
                        .unwrap_or_default(),
                };

                if activated.is_empty() {
                    continue;
                }
                *action_budget = (*action_budget).saturating_sub(activated.len() as u64);
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
                    && self
                        .runtime
                        .state
                        .run_status(&activation.run_id)
                        .is_some_and(|status| status == RunStatus::Running)
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

    fn complete(&mut self, action_budget: &mut u64) -> bool {
        let run_ids = self.runtime.state.runs.iter().cloned().collect::<Vec<_>>();
        let mut progressed = false;
        for run_id in run_ids {
            let status = self.runtime.state.run_status(&run_id);
            if matches!(
                status,
                Some(RunStatus::Completed | RunStatus::Failed | RunStatus::Stopped)
            ) {
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
            if activations.is_empty() {
                continue;
            }
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
                let completion_mode = self
                    .completion_modes
                    .get(&run_id)
                    .copied()
                    .unwrap_or(RunCompletionMode::Finite);
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
        if self.runtime.state.run_status(run_id) == Some(status) {
            return false;
        }
        self.runtime.append(EventPayload::RunStatusChanged {
            run_id: run_id.to_owned(),
            status,
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

fn route_for_each_artifact_key(for_each: &str) -> Option<&str> {
    for_each
        .trim()
        .strip_prefix("artifact.")
        .filter(|key| !key.is_empty())
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
