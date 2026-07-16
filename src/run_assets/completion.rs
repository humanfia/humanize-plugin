use super::{RunAssetActivation, RunAssetCompletion, RunAssetManifest};

pub(super) fn refresh_completion(manifest: &mut RunAssetManifest) {
    let mut expected = Vec::new();
    let mut complete = Vec::new();
    let mut incomplete = Vec::new();
    for (activation_id, activation) in &manifest.activations {
        expected.push(activation_id.clone());
        if activation_complete(activation) {
            complete.push(activation_id.clone());
        } else {
            incomplete.push(activation_id.clone());
        }
    }
    manifest.completion = RunAssetCompletion {
        flow_complete: manifest.flow.complete,
        expected_tmux_activations: expected,
        complete_tmux_activations: complete,
        incomplete_tmux_activations: incomplete.clone(),
        complete: manifest.flow.complete
            && incomplete.is_empty()
            && manifest.preservation_errors.is_empty()
            && !manifest.preservation_blocked,
    };
}

fn activation_complete(activation: &RunAssetActivation) -> bool {
    activation.pipe_acknowledged
        && activation.capture_phase == "complete"
        && activation.capture_complete
        && activation.ended_at_ms.is_some()
        && activation.termination_reason.is_some()
        && activation.preservation_status == "complete"
        && activation.resource_cleanup_status == "complete"
        && activation.final_capture_path.exists()
}

pub(super) fn activation_can_complete(activation: &RunAssetActivation) -> bool {
    activation.pipe_acknowledged
        && activation.capture_phase == "capturing"
        && activation.preservation_status == "capturing"
}
