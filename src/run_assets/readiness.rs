use std::fs::File;
use std::io::Read;

use sha2::{Digest, Sha256};

use super::journal::{self, JournalReadMode};
use super::*;

impl RunAssetStore {
    pub fn record_agent_ready_for_allocation(
        &self,
        run_id: &str,
        activation_id: &str,
        allocation_generation: u64,
        readiness_nonce: &str,
        pane_id: &str,
        source: &str,
    ) -> Result<bool, RunAssetError> {
        let mut manifest = self.load_manifest(run_id)?;
        let Some(mut activation) = manifest.activations.get(activation_id).cloned() else {
            return Ok(false);
        };
        if readiness_nonce.is_empty() {
            return Ok(false);
        }
        if activation.readiness_nonce.is_empty() {
            activation.readiness_nonce = readiness_nonce.to_string();
            manifest
                .activations
                .insert(activation_id.to_string(), activation.clone());
        }
        if activation.run_id != run_id
            || (!activation.pane_id.is_empty() && activation.pane_id != pane_id)
            || activation.allocation_generation != allocation_generation
            || activation.readiness_nonce.is_empty()
            || activation.readiness_nonce != readiness_nonce
            || activation.resource_cleanup_status == "complete"
        {
            return Ok(false);
        }
        let nonce_hash = readiness_nonce_hash(readiness_nonce);
        self.record_hook_fact(
            &mut manifest,
            HookFactInput {
                session_id: activation.session_id.clone(),
                activation_id: Some(activation.activation_id.clone()),
                hook: AGENT_READY_HOOK.to_string(),
                source_native_id: format!(
                    "agent_ready:{run_id}:{activation_id}:{allocation_generation}:{nonce_hash}:{source}"
                ),
                detail: HookFactDetail::AgentReady {
                    status: "ready".to_string(),
                    allocation_generation,
                    ready_signal_hash: Some(nonce_hash),
                },
                causal_id: None,
                correlation_id: None,
            },
        )?;
        Ok(true)
    }

    pub fn record_agent_readiness_failure(
        &self,
        manifest: &mut RunAssetManifest,
        activation_id: &str,
        _pane_id: &str,
        allocation_generation: u64,
        _source: &str,
        elapsed_ms: u64,
    ) -> Result<(), RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        let activation = manifest
            .activations
            .get(activation_id)
            .ok_or_else(|| RunAssetError::new(format!("activation not found: {activation_id}")))?
            .clone();
        if activation.allocation_generation != allocation_generation {
            return Err(RunAssetError::new(format!(
                "activation allocation generation mismatch: expected {}, got {allocation_generation}",
                activation.allocation_generation
            )));
        }
        let now = self.now_ms();
        self.record_hook_fact(
            manifest,
            HookFactInput {
                session_id: activation.session_id,
                activation_id: Some(activation.activation_id.clone()),
                hook: AGENT_READY_FAILURE_HOOK.to_string(),
                source_native_id: format!(
                    "agent_ready_failure:{}:{}:{}:{now}",
                    manifest.run_id, activation.activation_id, allocation_generation
                ),
                detail: HookFactDetail::AgentReadyFailure {
                    status: "timeout".to_string(),
                    allocation_generation,
                    elapsed_ms,
                },
                causal_id: None,
                correlation_id: None,
            },
        )?;
        Ok(())
    }

    pub fn agent_ready_signal(
        &self,
        manifest: &RunAssetManifest,
        activation_id: &str,
        pane_id: &str,
        allocation_generation: u64,
    ) -> Result<Option<Value>, RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        let Some(activation) = manifest.activations.get(activation_id) else {
            return Ok(None);
        };
        if (!activation.pane_id.is_empty() && activation.pane_id != pane_id)
            || activation.allocation_generation != allocation_generation
        {
            return Ok(None);
        }
        let expected_nonce_hash = (!activation.readiness_nonce.is_empty())
            .then(|| readiness_nonce_hash(&activation.readiness_nonce));
        for event in journal::read_events(manifest, JournalReadMode::RecoverTornTail)? {
            if event.kind != "hook.observed" {
                continue;
            }
            let fact = &event.data["fact"];
            if fact["hook"] != AGENT_READY_HOOK {
                continue;
            }
            if fact["activation_id"].as_str() != Some(activation_id) {
                continue;
            }
            let observed = &fact["observed"];
            if observed["allocation_generation"].as_u64().unwrap_or(0) == allocation_generation
                && expected_nonce_hash
                    .as_deref()
                    .map(|hash| observed["ready_signal_hash"].as_str() == Some(hash))
                    .unwrap_or(true)
            {
                return Ok(Some(observed.clone()));
            }
        }
        Ok(None)
    }

    pub fn ensure_activation_readiness_nonce(
        &self,
        manifest: &mut RunAssetManifest,
        activation_id: &str,
    ) -> Result<String, RunAssetError> {
        self.validate_manifest_authority(manifest)?;
        let activation = manifest
            .activations
            .get(activation_id)
            .ok_or_else(|| RunAssetError::new(format!("activation not found: {activation_id}")))?;
        if !activation.readiness_nonce.is_empty() {
            return Ok(activation.readiness_nonce.clone());
        }
        let mut candidate = manifest.clone();
        let readiness_nonce = random_private_nonce()?;
        let activation = candidate
            .activations
            .get_mut(activation_id)
            .expect("validated activation should exist");
        activation.readiness_nonce = readiness_nonce.clone();
        write_activation_metadata_file(activation)?;
        candidate.updated_at_ms = self.now_ms();
        write_manifest_file(&candidate)?;
        *manifest = candidate;
        Ok(readiness_nonce)
    }
}

pub(super) fn random_private_nonce() -> Result<String, RunAssetError> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|err| RunAssetError::new(format!("read OS randomness failed: {err}")))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn readiness_nonce_hash(nonce: &str) -> String {
    let digest = Sha256::digest(nonce.as_bytes());
    format!("sha256:{digest:x}")
}
