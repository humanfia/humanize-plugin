use std::time::Duration;

use crate::input_ledger::{
    MachineInputClock, MachineInputLedger, MachineInputRecord, MachineInputSubmission,
    machine_input_payload_hash, machine_input_transaction_id,
};

use super::{
    CommandRunner, TmuxActivationMetadata, TmuxAdapter, TmuxError, TmuxPane, sleep_if_needed,
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
            text,
            false,
            None,
            submit_key_count,
            &self.input_config,
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
            text,
            false,
            Some(projection),
            submit_key_count,
            &self.input_config,
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
            text,
            true,
            None,
            submit_key_count,
            &self.input_config,
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
            text,
            false,
            None,
            input_config.submit_key_count,
            input_config,
        )
    }

    fn send_input_transaction_with_options(
        &self,
        metadata: &TmuxActivationMetadata,
        text: &str,
        clear_before_input: bool,
        projection: Option<&str>,
        submit_key_count: usize,
        input_config: &TmuxInputTransactionConfig,
    ) -> Result<TmuxInputTransaction, TmuxError> {
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
        let input_result = if requires_bracketed_paste(text) {
            let buffer_name = attempt.transaction_id.replace(':', "-");
            self.paste_keys_literal(&pane, text, &buffer_name)
        } else {
            self.send_keys_literal(&pane, text)
        };
        if let Err(err) = input_result {
            self.record_failed_input(&attempt, input_config);
            return Err(err);
        }
        sleep_if_needed(input_config.prompt_to_submit_delay(text));
        for index in 0..submit_key_count {
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
        Ok(TmuxInputTransaction { record })
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
}

impl TmuxInputTransaction {
    pub fn transaction_id(&self) -> &str {
        &self.record.transaction_id
    }

    pub fn record(&self) -> &MachineInputRecord {
        &self.record
    }
}

fn requires_bracketed_paste(text: &str) -> bool {
    text.len() >= 512 || text.contains(['\r', '\n'])
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
