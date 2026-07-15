use super::*;

pub(super) fn canonicalize_draft(draft: &FlowDraft) -> FlowDraft {
    let mut draft = draft.clone();

    for node in &mut draft.nodes {
        node.write_scopes.sort();
        node.write_scopes.dedup();
        node.extensions.sort();
        node.extensions.dedup();
        if let Some(action) = &mut node.action {
            sort_unique(&mut action.resource_refs);
            sort_unique(&mut action.reads);
            sort_unique(&mut action.writes);
        }
    }
    draft.nodes.sort_by(|left, right| left.id.cmp(&right.id));

    for contract in &mut draft.contracts {
        contract.artifacts.sort_by(|left, right| {
            left.id
                .cmp(&right.id)
                .then(left.schema_resource_id.cmp(&right.schema_resource_id))
        });
        contract.artifacts.dedup();
    }
    draft
        .contracts
        .sort_by(|left, right| left.id.cmp(&right.id));

    draft.routes.sort_by(|left, right| {
        left.activate
            .cmp(&right.activate)
            .then(left.predicate.cmp(&right.predicate))
            .then(left.for_each.cmp(&right.for_each))
    });
    draft.resources.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.kind.as_str().cmp(right.kind.as_str()))
            .then(left.source.cmp(&right.source))
    });
    draft.imports.sort_by(|left, right| {
        left.resource_id
            .cmp(&right.resource_id)
            .then(left.alias.cmp(&right.alias))
    });
    draft.imports.dedup();
    draft.policies.write_scopes.sort();
    draft.policies.write_scopes.dedup();
    draft.extensions.sort();
    draft.extensions.dedup();

    draft
}

fn sort_unique(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}
