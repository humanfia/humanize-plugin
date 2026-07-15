use serde_json::json;
use sha2::{Digest, Sha256};

use super::{RunAssetActivation, RunAssetError, atomic_write_private, durable_fs};

pub(super) fn redact_public_manifest_secrets(
    object: &mut serde_json::Map<String, serde_json::Value>,
) {
    if let Some(activations) = object
        .get_mut("activations")
        .and_then(serde_json::Value::as_object_mut)
    {
        for activation in activations.values_mut() {
            let Some(activation) = activation.as_object_mut() else {
                continue;
            };
            redact_public_activation_secrets(activation);
        }
    }
}

pub(super) fn public_hash_ref(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}

pub(super) fn write_activation_metadata_file(
    activation: &RunAssetActivation,
) -> Result<(), RunAssetError> {
    if let Some(parent) = activation.metadata_path.parent() {
        durable_fs::create_dir_all(parent)?;
        durable_fs::ensure_private_dir(parent)?;
    }
    let mut value = serde_json::to_value(activation).map_err(|err| {
        RunAssetError::new(format!("serialize activation metadata failed: {err}"))
    })?;
    if let serde_json::Value::Object(object) = &mut value {
        redact_public_activation_secrets(object);
    }
    let payload = serde_json::to_string_pretty(&value).map_err(|err| {
        RunAssetError::new(format!("serialize activation metadata failed: {err}"))
    })?;
    atomic_write_private(&activation.metadata_path, payload.as_bytes()).map_err(|err| {
        RunAssetError::new(format!(
            "write activation metadata {} failed: {err}",
            activation.metadata_path.display()
        ))
    })
}

fn redact_public_activation_secrets(object: &mut serde_json::Map<String, serde_json::Value>) {
    object.remove("readiness_nonce");
    for (field, public_ref) in [
        ("tmux_target", "tmux_target_ref"),
        ("session_id", "session_ref"),
        ("window_id", "window_ref"),
        ("window_name", "window_name_ref"),
        ("pane_id", "pane_ref"),
    ] {
        if let Some(value) = object.remove(field).and_then(|value| {
            value
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        }) {
            object.insert(public_ref.to_string(), json!(public_hash_ref(&value)));
        }
    }
}
