use std::time::Duration;

use crate::input_ledger::{
    MachineInputClock, MachineInputLedger, MachineInputRecord, MachineInputSubmission,
    machine_input_payload_hash, machine_input_transaction_id,
};

use super::{
    CommandRunner, TmuxActivationMetadata, TmuxAdapter, TmuxError, TmuxPane,
    inferred_harness_profile, sleep_if_needed,
};

struct MachineInputAttempt<'a> {
    metadata: &'a TmuxActivationMetadata,
    text: &'a str,
    projection: Option<&'a str>,
    started_at_ms: u64,
    transaction_id: String,
    submit_key_count: usize,
}

impl MachineInputAttempt<'_> {
    fn submission(&self, submitted_at_ms: u64) -> MachineInputSubmission<'_> {
        MachineInputSubmission {
            run_id: self.metadata.run_id(),
            activation_id: self.metadata.activation_id(),
            pane_id: self.metadata.pane_id(),
            allocation_generation: self.metadata.allocation_generation(),
            started_at_ms: self.started_at_ms,
            submitted_at_ms,
            text: self.text,
            submit_key_count: self.submit_key_count,
            transaction_id: self.transaction_id.clone(),
        }
    }
}

impl<R: CommandRunner> TmuxAdapter<R> {
    pub fn send_input_transaction(
        &self,
        metadata: &TmuxActivationMetadata,
        text: &str,
    ) -> Result<TmuxInputTransaction, TmuxError> {
        self.send_input_transaction_with_submit_key_count(
            metadata,
            text,
            self.input_config.submit_key_count,
        )
    }

    pub fn send_input_transaction_with_submit_key_count(
        &self,
        metadata: &TmuxActivationMetadata,
        text: &str,
        submit_key_count: usize,
    ) -> Result<TmuxInputTransaction, TmuxError> {
        self.send_input_transaction_with_options(
            metadata,
            InputTransactionRequest {
                text,
                clear_before_input: false,
                projection: None,
                submit_key_count,
                acceptance: None,
                input_config: &self.input_config,
            },
        )
    }

    pub fn send_input_transaction_with_projection(
        &self,
        metadata: &TmuxActivationMetadata,
        text: &str,
        projection: &str,
    ) -> Result<TmuxInputTransaction, TmuxError> {
        self.send_input_transaction_with_projection_and_submit_key_count(
            metadata,
            text,
            projection,
            self.input_config.submit_key_count,
        )
    }

    pub fn send_input_transaction_with_projection_and_submit_key_count(
        &self,
        metadata: &TmuxActivationMetadata,
        text: &str,
        projection: &str,
        submit_key_count: usize,
    ) -> Result<TmuxInputTransaction, TmuxError> {
        self.send_input_transaction_with_options(
            metadata,
            InputTransactionRequest {
                text,
                clear_before_input: false,
                projection: Some(projection),
                submit_key_count,
                acceptance: None,
                input_config: &self.input_config,
            },
        )
    }

    pub fn send_clean_input_transaction(
        &self,
        metadata: &TmuxActivationMetadata,
        text: &str,
    ) -> Result<TmuxInputTransaction, TmuxError> {
        self.send_clean_input_transaction_with_submit_key_count(
            metadata,
            text,
            self.input_config.submit_key_count,
        )
    }

    pub fn send_clean_input_transaction_with_submit_key_count(
        &self,
        metadata: &TmuxActivationMetadata,
        text: &str,
        submit_key_count: usize,
    ) -> Result<TmuxInputTransaction, TmuxError> {
        self.send_input_transaction_with_options(
            metadata,
            InputTransactionRequest {
                text,
                clear_before_input: true,
                projection: None,
                submit_key_count,
                acceptance: None,
                input_config: &self.input_config,
            },
        )
    }

    pub(crate) fn send_clean_input_transaction_with_agent_acceptance(
        &self,
        metadata: &TmuxActivationMetadata,
        text: &str,
        submit_key_count: usize,
        agent_command: &str,
        acceptance_timeout: Duration,
    ) -> Result<TmuxInputTransaction, TmuxError> {
        self.send_input_transaction_with_options(
            metadata,
            InputTransactionRequest {
                text,
                clear_before_input: true,
                projection: None,
                submit_key_count,
                acceptance: Some(AgentSubmissionAcceptance {
                    agent_command,
                    timeout: acceptance_timeout,
                }),
                input_config: &self.input_config,
            },
        )
    }

    pub fn send_input_transaction_with_config(
        &self,
        metadata: &TmuxActivationMetadata,
        text: &str,
        input_config: &TmuxInputTransactionConfig,
    ) -> Result<TmuxInputTransaction, TmuxError> {
        self.send_input_transaction_with_options(
            metadata,
            InputTransactionRequest {
                text,
                clear_before_input: false,
                projection: None,
                submit_key_count: input_config.submit_key_count,
                acceptance: None,
                input_config,
            },
        )
    }

    fn send_input_transaction_with_options(
        &self,
        metadata: &TmuxActivationMetadata,
        request: InputTransactionRequest<'_>,
    ) -> Result<TmuxInputTransaction, TmuxError> {
        let InputTransactionRequest {
            text,
            clear_before_input,
            projection,
            submit_key_count,
            acceptance,
            input_config,
        } = request;
        self.validate_exact_pane(metadata)?;
        let submit_key_count = submit_key_count.max(1);
        let started_at_ms = input_config.clock.now_ms();
        let payload_hash = machine_input_payload_hash(text);
        let sequence = input_config.ledger.next_sequence();
        let transaction_id = machine_input_transaction_id(
            metadata.run_id(),
            metadata.activation_id(),
            metadata.pane_id(),
            metadata.allocation_generation(),
            &payload_hash,
            started_at_ms,
            sequence,
        );
        let attempt = MachineInputAttempt {
            metadata,
            text,
            projection,
            started_at_ms,
            transaction_id,
            submit_key_count,
        };
        let pane = TmuxPane::new_in_session(
            metadata.session_id(),
            metadata.window_id(),
            metadata.activation_id(),
            metadata.pane_id(),
        );

        input_config
            .ledger
            .append(match attempt.projection {
                Some(projection) => MachineInputRecord::started_with_projection(
                    attempt.submission(started_at_ms),
                    projection,
                ),
                None => MachineInputRecord::started(attempt.submission(started_at_ms)),
            })
            .map_err(|err| TmuxError::input_ledger(&err.to_string()))?;

        if clear_before_input && let Err(err) = self.send_key(&pane, "C-u") {
            self.record_failed_input(&attempt, input_config);
            return Err(err);
        }
        let buffer_name = attempt.transaction_id.replace(':', "-");
        let input_result = self.paste_keys_literal(metadata, &pane, text, &buffer_name);
        if let Err(err) = input_result {
            self.record_failed_input(&attempt, input_config);
            return Err(err);
        }
        sleep_if_needed(input_config.prompt_to_submit_delay(text));
        for index in 0..submit_key_count {
            if let Err(err) = self.validate_exact_pane(metadata) {
                self.record_failed_input(&attempt, input_config);
                return Err(err);
            }
            if let Err(err) = self.send_key(&pane, "Enter") {
                self.record_failed_input(&attempt, input_config);
                return Err(err);
            }
            if index + 1 < submit_key_count {
                sleep_if_needed(input_config.submit_key_delay);
            }
        }
        let submitted_at_ms = input_config.clock.now_ms();
        let submission = attempt.submission(submitted_at_ms);
        let record = match attempt.projection {
            Some(projection) => {
                MachineInputRecord::submitted_with_projection(submission, projection)
            }
            None => MachineInputRecord::submitted(submission),
        };
        input_config
            .ledger
            .append(record.clone())
            .map_err(|err| TmuxError::input_ledger(&err.to_string()))?;
        let mut submission_acceptance = None;
        if let Some(acceptance) = acceptance
            && let Some(profile) = inferred_harness_profile(acceptance.agent_command)
            && self
                .wait_for_pane_text_case_insensitive(
                    metadata,
                    profile.acceptance_pattern(),
                    acceptance.timeout,
                )
                .is_ok()
        {
            submission_acceptance = Some(TmuxSubmissionAcceptance {
                profile: profile.name(),
                signal: "working_state",
            });
        }
        Ok(TmuxInputTransaction {
            record,
            acceptance: submission_acceptance,
        })
    }

    fn record_failed_input(
        &self,
        attempt: &MachineInputAttempt<'_>,
        input_config: &TmuxInputTransactionConfig,
    ) {
        let failed_at_ms = input_config.clock.now_ms();
        let _ = input_config.ledger.append(match attempt.projection {
            Some(projection) => MachineInputRecord::failed_with_projection(
                attempt.submission(failed_at_ms),
                projection,
            ),
            None => MachineInputRecord::failed(attempt.submission(failed_at_ms)),
        });
    }
}

struct AgentSubmissionAcceptance<'a> {
    agent_command: &'a str,
    timeout: Duration,
}

struct InputTransactionRequest<'a> {
    text: &'a str,
    clear_before_input: bool,
    projection: Option<&'a str>,
    submit_key_count: usize,
    acceptance: Option<AgentSubmissionAcceptance<'a>>,
    input_config: &'a TmuxInputTransactionConfig,
}

#[derive(Debug, Clone)]
pub struct TmuxInputTransactionConfig {
    ledger: MachineInputLedger,
    clock: MachineInputClock,
    submit_key_count: usize,
    prompt_to_submit_delay: Duration,
    prompt_byte_delay: Duration,
    max_prompt_to_submit_delay: Duration,
    submit_key_delay: Duration,
}

impl TmuxInputTransactionConfig {
    pub fn runtime() -> Self {
        Self::runtime_with_ledger(MachineInputLedger::runtime_default())
    }

    pub fn runtime_with_ledger(ledger: MachineInputLedger) -> Self {
        Self {
            ledger,
            clock: MachineInputClock::realtime(),
            submit_key_count: 1,
            prompt_to_submit_delay: Duration::from_millis(250),
            prompt_byte_delay: Duration::from_millis(3),
            max_prompt_to_submit_delay: Duration::from_secs(30),
            submit_key_delay: Duration::from_millis(250),
        }
    }

    pub fn deterministic(ledger: MachineInputLedger, timestamp_ms: u64) -> Self {
        Self {
            ledger,
            clock: MachineInputClock::fixed(timestamp_ms),
            submit_key_count: 1,
            prompt_to_submit_delay: Duration::ZERO,
            prompt_byte_delay: Duration::ZERO,
            max_prompt_to_submit_delay: Duration::ZERO,
            submit_key_delay: Duration::ZERO,
        }
    }

    pub fn with_ledger(mut self, ledger: MachineInputLedger) -> Self {
        self.ledger = ledger;
        self
    }

    pub fn with_submit_key_count(mut self, submit_key_count: usize) -> Self {
        self.submit_key_count = submit_key_count.max(1);
        self
    }

    pub fn with_prompt_to_submit_delay(mut self, delay: Duration) -> Self {
        self.prompt_to_submit_delay = delay;
        self
    }

    pub fn with_prompt_byte_delay(mut self, delay: Duration) -> Self {
        self.prompt_byte_delay = delay;
        self
    }

    pub fn with_max_prompt_to_submit_delay(mut self, delay: Duration) -> Self {
        self.max_prompt_to_submit_delay = delay;
        self
    }

    pub fn with_submit_key_delay(mut self, delay: Duration) -> Self {
        self.submit_key_delay = delay;
        self
    }

    fn prompt_to_submit_delay(&self, text: &str) -> Duration {
        let byte_count = u32::try_from(text.len()).unwrap_or(u32::MAX);
        let scaled_delay = self
            .prompt_byte_delay
            .saturating_mul(byte_count)
            .min(self.max_prompt_to_submit_delay);
        self.prompt_to_submit_delay.max(scaled_delay)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TmuxInputTransaction {
    record: MachineInputRecord,
    acceptance: Option<TmuxSubmissionAcceptance>,
}

impl TmuxInputTransaction {
    pub fn transaction_id(&self) -> &str {
        &self.record.transaction_id
    }

    pub fn record(&self) -> &MachineInputRecord {
        &self.record
    }

    pub fn acceptance(&self) -> Option<&TmuxSubmissionAcceptance> {
        self.acceptance.as_ref()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TmuxSubmissionAcceptance {
    profile: &'static str,
    signal: &'static str,
}

impl TmuxSubmissionAcceptance {
    pub fn profile(&self) -> &'static str {
        self.profile
    }

    pub fn signal(&self) -> &'static str {
        self.signal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_prompt_delay_scales_with_input_and_caps_at_thirty_seconds() {
        let runtime = TmuxInputTransactionConfig::runtime();

        assert_eq!(
            runtime.prompt_to_submit_delay("short"),
            Duration::from_millis(250)
        );
        assert_eq!(
            runtime.prompt_to_submit_delay(&"x".repeat(1_000)),
            Duration::from_secs(3)
        );
        assert_eq!(
            runtime.prompt_to_submit_delay(&"x".repeat(20_000)),
            Duration::from_secs(30)
        );
        assert_eq!(
            TmuxInputTransactionConfig::deterministic(MachineInputLedger::in_memory(), 0)
                .prompt_to_submit_delay(&"x".repeat(20_000)),
            Duration::ZERO
        );
    }
}
