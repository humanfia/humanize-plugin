use super::{NodeSpec, RuntimeState};

pub(super) fn activation_id_for(
    node: &NodeSpec,
    stable_key: Option<&str>,
    generation: u64,
) -> String {
    activation_id_for_node(node.id(), stable_key, generation)
}

fn activation_id_for_node(node_id: &str, stable_key: Option<&str>, generation: u64) -> String {
    let base = match stable_key {
        Some(stable_key) => format!("{node_id}:{stable_key}"),
        None => node_id.to_owned(),
    };
    if generation == 0 {
        base
    } else {
        format!("{base}~g{generation}")
    }
}

pub(super) fn next_activation_identity(
    state: &RuntimeState,
    run_id: &str,
    node_id: &str,
    stable_key: Option<&str>,
) -> (u64, String) {
    let generation = state.next_activation_generation(run_id, node_id, stable_key);
    (
        generation,
        activation_id_for_node(node_id, stable_key, generation),
    )
}

pub(super) fn activation_key(run_id: &str, activation_id: &str) -> (String, String) {
    (run_id.to_owned(), activation_id.to_owned())
}

pub(super) fn slot_index_key(run_id: &str, artifact_key: &str) -> (String, String) {
    (run_id.to_owned(), artifact_key.to_owned())
}

pub(super) fn effect_index_key(
    run_id: &str,
    activation_id: &str,
    effect_key: &str,
) -> (String, String, String) {
    (
        run_id.to_owned(),
        activation_id.to_owned(),
        effect_key.to_owned(),
    )
}

pub(super) fn stop_fact_id(run_id: &str, activation_id: &str, event_sequence: u64) -> String {
    format!("{run_id}/{activation_id}/{event_sequence}")
}

pub(super) fn artifact_id(event_sequence: u64) -> String {
    format!("artifact:{event_sequence}")
}

pub(super) fn flow_lock_application_id(event_sequence: u64) -> String {
    format!("flow-lock-application:{event_sequence}")
}

pub(super) fn content_hash(payload: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in payload.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}
