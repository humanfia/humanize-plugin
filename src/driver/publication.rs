use std::io;
use std::path::Path;

use crate::run_assets::RunAssetManifest;
use crate::run_assets::publication::{self, PublicationMutation, PublicationTransaction};
use crate::runtime;

use super::{
    DriverFailure, RUNTIME_EVENT_BATCH_PROTOCOL, RuntimeDriverService, RuntimeEventBatchRecord,
};

pub(super) fn recover_runtime_events(private_run_root: &Path) -> io::Result<Vec<runtime::Event>> {
    let path = private_run_root.join("driver").join(super::EVENTS_FILE);
    let mut events = super::persistence::read_runtime_events(private_run_root)?;
    for transaction in
        publication::pending_transactions(private_run_root).map_err(run_asset_io_error)?
    {
        if let PublicationMutation::RuntimeEvents {
            base_event_count,
            events: pending_events,
        } = transaction.mutation()
        {
            reconcile_private_runtime_batch(&path, &mut events, *base_event_count, pending_events)?;
        }
    }
    Ok(events)
}

impl RuntimeDriverService {
    pub(super) fn private_runtime_is_terminal(&self) -> bool {
        matches!(
            self.run_status(),
            runtime::RunStatus::Completed
                | runtime::RunStatus::Failed
                | runtime::RunStatus::Stopped
        )
    }

    pub(super) fn commit_runtime_with_publication(
        &mut self,
        next_driver: runtime::DriverState,
        routes: &[runtime::RouteDecision],
    ) -> Result<(), DriverFailure> {
        self.reconcile_publication_outbox()?;
        let events = next_driver.runtime().events();
        if self.persisted_event_count >= events.len() {
            self.driver = next_driver;
            let _ = self.write_snapshot();
            return Ok(());
        }
        let base_event_count = self.persisted_event_count;
        let pending_events = events[base_event_count..].to_vec();
        self.fail_runtime_append_if_requested(&pending_events)?;
        let manifest = self.load_run_asset_manifest()?;
        self.run_asset_store
            .reconcile_public_seal(&manifest, self.private_runtime_is_terminal(), true)
            .map_err(DriverFailure::from_run_asset)?;
        let public_records = self
            .run_asset_store
            .prepare_runtime_publication(&manifest, &pending_events, routes)
            .map_err(DriverFailure::from_run_asset)?;
        self.run_asset_store
            .preflight_publication(&manifest, &public_records)
            .map_err(|err| DriverFailure::new("publication_blocked", err.to_string()))?;
        let transaction =
            PublicationTransaction::runtime(base_event_count, pending_events, public_records)
                .map_err(|err| DriverFailure::new("publication_blocked", err.to_string()))?;
        let transaction = publication::persist_pending(&self.private_run_root, transaction)
            .map_err(|err| DriverFailure::new("publication_blocked", err.to_string()))?;

        if let Err(err) = self.append_private_runtime_transaction(&transaction) {
            self.publication_blocked = Some(err.message.clone());
            return Err(err);
        }
        let end_event_count = runtime_end_event_count(&transaction)?;
        self.persisted_event_count = end_event_count;
        self.driver = next_driver;
        if let Err(err) = self.publish_transaction(&manifest, &transaction) {
            self.publication_blocked = Some(err.message.clone());
            return Err(err);
        }
        if let Err(err) = self.acknowledge_transaction(&transaction) {
            self.publication_blocked = Some(err.message.clone());
            return Err(err);
        }
        let _ = self.write_snapshot();
        self.publication_blocked = None;
        Ok(())
    }

    pub(super) fn reconcile_publication_outbox(&mut self) -> Result<(), DriverFailure> {
        let pending = publication::pending_transactions(&self.private_run_root)
            .map_err(|err| DriverFailure::new("publication_blocked", err.to_string()))?;
        if pending.is_empty() {
            self.publication_blocked = None;
            return Ok(());
        }
        for transaction in pending {
            let manifest = self.manifest_for_transaction(&transaction)?;
            self.run_asset_store
                .reconcile_public_seal(&manifest, false, false)
                .map_err(|err| DriverFailure::new("publication_blocked", err.to_string()))?;
            self.reconcile_private_transaction(&transaction)?;
            self.publish_transaction(&manifest, &transaction)?;
            self.acknowledge_transaction(&transaction)?;
        }
        self.refresh_published_record_sources()?;
        let _ = self.write_snapshot();
        self.publication_blocked = None;
        Ok(())
    }

    pub(super) fn repair_publication_projections(&self) -> Result<(), DriverFailure> {
        for transaction in publication::published_transactions(&self.private_run_root)
            .map_err(|err| DriverFailure::new("publication_blocked", err.to_string()))?
        {
            let manifest = self.manifest_for_transaction(&transaction)?;
            self.publish_transaction(&manifest, &transaction)?;
        }
        let manifest = self.load_run_asset_manifest()?;
        self.run_asset_store
            .repair_public_manifest_projection(&manifest)
            .map_err(DriverFailure::from_run_asset)
    }

    fn reconcile_private_transaction(
        &mut self,
        transaction: &PublicationTransaction,
    ) -> Result<(), DriverFailure> {
        match transaction.mutation() {
            PublicationMutation::RuntimeEvents {
                base_event_count,
                events,
            } => {
                let mut current = super::persistence::read_runtime_events(&self.private_run_root)
                    .map_err(|err| DriverFailure::io("publication_blocked", err))?;
                reconcile_private_runtime_batch(
                    &self.events_path(),
                    &mut current,
                    *base_event_count,
                    events,
                )
                .map_err(|err| DriverFailure::io("publication_blocked", err))?;
                self.persisted_event_count = current.len();
                self.driver =
                    runtime::DriverState::from_runtime(runtime::Runtime::from_events(current));
                Ok(())
            }
            PublicationMutation::RunAssetManifest { .. } => self
                .run_asset_store
                .reconcile_private_manifest_transaction(transaction)
                .map_err(|err| DriverFailure::new("publication_blocked", err.to_string())),
        }
    }

    fn append_private_runtime_transaction(
        &self,
        transaction: &PublicationTransaction,
    ) -> Result<(), DriverFailure> {
        let PublicationMutation::RuntimeEvents {
            base_event_count,
            events,
        } = transaction.mutation()
        else {
            return Err(DriverFailure::new(
                "persistence_failed",
                "runtime append received a run asset transaction",
            ));
        };
        let batch = RuntimeEventBatchRecord {
            protocol: RUNTIME_EVENT_BATCH_PROTOCOL,
            base_event_count: *base_event_count,
            events: events.clone(),
        };
        super::storage::append_json_line_private(&self.events_path(), &batch)
    }

    fn publish_transaction(
        &self,
        manifest: &RunAssetManifest,
        transaction: &PublicationTransaction,
    ) -> Result<(), DriverFailure> {
        self.run_asset_store
            .publish_record_batch_and_projection(manifest, transaction.public_records())
            .map_err(|err| DriverFailure::new("publication_blocked", err.to_string()))
    }

    fn acknowledge_transaction(
        &self,
        transaction: &PublicationTransaction,
    ) -> Result<(), DriverFailure> {
        publication::acknowledge(&self.private_run_root, transaction)
            .map_err(|err| DriverFailure::new("publication_blocked", err.to_string()))
    }

    fn manifest_for_transaction(
        &self,
        transaction: &PublicationTransaction,
    ) -> Result<RunAssetManifest, DriverFailure> {
        match transaction.mutation() {
            PublicationMutation::RunAssetManifest { manifest, .. } => Ok((**manifest).clone()),
            PublicationMutation::RuntimeEvents { .. } => self.load_run_asset_manifest(),
        }
    }
}

fn runtime_end_event_count(transaction: &PublicationTransaction) -> Result<usize, DriverFailure> {
    let PublicationMutation::RuntimeEvents {
        base_event_count,
        events,
    } = transaction.mutation()
    else {
        return Err(DriverFailure::new(
            "persistence_failed",
            "runtime publication transaction has the wrong mutation kind",
        ));
    };
    Ok(base_event_count.saturating_add(events.len()))
}

fn reconcile_private_runtime_batch(
    events_path: &Path,
    current: &mut Vec<runtime::Event>,
    base_event_count: usize,
    pending_events: &[runtime::Event],
) -> io::Result<()> {
    let end_event_count = base_event_count.saturating_add(pending_events.len());
    if current.len() == base_event_count {
        let batch = RuntimeEventBatchRecord {
            protocol: RUNTIME_EVENT_BATCH_PROTOCOL,
            base_event_count,
            events: pending_events.to_vec(),
        };
        super::storage::append_json_line_private(events_path, &batch)
            .map_err(|err| io::Error::other(err.message))?;
        current.extend_from_slice(pending_events);
        return Ok(());
    }
    if current.len() < end_event_count {
        return Err(invalid_data(
            "private runtime event log ends inside a publication transaction",
        ));
    }
    if current[base_event_count..end_event_count] != *pending_events {
        return Err(invalid_data(
            "private runtime events conflict with the publication outbox",
        ));
    }
    Ok(())
}

fn run_asset_io_error(error: crate::run_assets::RunAssetError) -> io::Error {
    io::Error::other(error.to_string())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
