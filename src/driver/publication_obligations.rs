use crate::input_ledger::{MachineInputRecord, MachineInputStatus};
use crate::run_assets::publication;
use crate::run_assets::{
    SessionRelation, machine_input_source_native_id, session_source_native_id,
};

use super::delivery::{
    DELIVERY_ROLE_AGENT_LAUNCH, DELIVERY_ROLE_NODE_PROMPT, DELIVERY_ROLE_PARTICIPANT_MESSAGE,
};
use super::storage::read_jsonl_recover_torn_tail;
use super::{DriverFailure, RuntimeDriverService};

impl RuntimeDriverService {
    pub(super) fn refresh_published_record_sources(&mut self) -> Result<(), DriverFailure> {
        self.published_record_sources =
            publication::published_source_native_ids(&self.private_run_root)
                .map_err(|err| DriverFailure::new("publication_blocked", err.to_string()))?;
        Ok(())
    }

    pub(super) fn reconcile_publication_obligations(&mut self) -> Result<(), DriverFailure> {
        self.reconcile_flow_revision_publications()?;
        self.reconcile_participant_publications()?;
        self.reconcile_machine_input_publications()
    }

    fn reconcile_flow_revision_publications(&self) -> Result<(), DriverFailure> {
        let packages = self.locks.values().cloned().collect::<Vec<_>>();
        for package in packages {
            self.publish_run_asset_flow_revision(&package)?;
        }
        Ok(())
    }

    pub(super) fn reconcile_participant_publications(&mut self) -> Result<(), DriverFailure> {
        let bindings = self
            .participant_bindings
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for binding in bindings {
            let Some(native_session_id) = binding.native_session_id.as_deref() else {
                continue;
            };
            let platform = binding.platform.as_deref().ok_or_else(|| {
                DriverFailure::new(
                    "publication_blocked",
                    "bound participant is missing its native platform",
                )
            })?;
            let source = binding.source.as_deref().ok_or_else(|| {
                DriverFailure::new(
                    "publication_blocked",
                    "bound participant is missing its native source",
                )
            })?;
            let bound_source = session_source_native_id(
                native_session_id,
                SessionRelation::Executes,
                &binding.activation_id,
            );
            if !self.published_record_sources.contains(&bound_source) {
                let projected = self
                    .run_asset_store
                    .record_agent_ready_for_allocation(
                        &self.config.run_id,
                        &binding.activation_id,
                        binding.allocation_generation,
                        &binding.readiness_nonce,
                        &binding.pane_id,
                        source,
                    )
                    .map_err(DriverFailure::from_run_asset)?;
                if !projected {
                    return Err(DriverFailure::new(
                        "publication_blocked",
                        "native participant binding could not be projected",
                    ));
                }

                let started_source = session_source_native_id(
                    native_session_id,
                    SessionRelation::Orchestrates,
                    &self.config.run_id,
                );
                if !self.published_record_sources.contains(&started_source) {
                    let mut manifest = self.load_run_asset_manifest()?;
                    self.run_asset_store
                        .record_session_association(
                            &mut manifest,
                            native_session_id,
                            SessionRelation::Orchestrates,
                            None,
                            platform,
                            None,
                        )
                        .map_err(DriverFailure::from_run_asset)?;
                    self.published_record_sources.insert(started_source);
                }

                let mut manifest = self.load_run_asset_manifest()?;
                self.run_asset_store
                    .record_session_association(
                        &mut manifest,
                        native_session_id,
                        SessionRelation::Executes,
                        Some(&binding.activation_id),
                        platform,
                        None,
                    )
                    .map_err(DriverFailure::from_run_asset)?;
                self.published_record_sources.insert(bound_source);
            }

            let Some(exit_status) = binding.exit_status else {
                continue;
            };
            let ended_source = session_source_native_id(
                native_session_id,
                SessionRelation::Ended,
                &binding.activation_id,
            );
            if self.published_record_sources.contains(&ended_source) {
                continue;
            }
            let mut manifest = self.load_run_asset_manifest()?;
            self.run_asset_store
                .record_session_association(
                    &mut manifest,
                    native_session_id,
                    SessionRelation::Ended,
                    Some(&binding.activation_id),
                    platform,
                    Some(exit_status),
                )
                .map_err(DriverFailure::from_run_asset)?;
            self.published_record_sources.insert(ended_source);
        }
        Ok(())
    }

    fn reconcile_machine_input_publications(&mut self) -> Result<(), DriverFailure> {
        let path = self.driver_dir().join("machine-inputs.jsonl");
        let records = read_jsonl_recover_torn_tail::<MachineInputRecord>(&path)
            .map_err(|err| DriverFailure::new("publication_blocked", err.to_string()))?;
        let deliveries = self
            .ambiguous_deliveries
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for delivery in deliveries {
            if !matches!(
                delivery.role.as_str(),
                DELIVERY_ROLE_AGENT_LAUNCH
                    | DELIVERY_ROLE_NODE_PROMPT
                    | DELIVERY_ROLE_PARTICIPANT_MESSAGE
            ) {
                return Err(DriverFailure::new(
                    "publication_blocked",
                    "private input delivery has an unknown role",
                ));
            }
            let Some(record) = records.iter().rev().find(|record| {
                record.status == MachineInputStatus::Submitted
                    && record.run_id == self.config.run_id
                    && record.activation_id == delivery.activation_id
                    && record.pane_id == delivery.pane_id
                    && record.allocation_generation == delivery.allocation_generation
                    && record.payload_hash == delivery.payload_hash
            }) else {
                continue;
            };
            let source_native_id = machine_input_source_native_id(&record.transaction_id);
            if self.published_record_sources.contains(&source_native_id) {
                continue;
            }
            self.record_machine_input(&delivery.role, record)?;
        }
        Ok(())
    }
}
