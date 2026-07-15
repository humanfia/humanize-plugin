use super::*;

pub fn flow_suggest(input: FlowSuggestInput) -> Result<FlowDraft, FlowSuggestError> {
    let goal = input.goal.trim();
    if goal.is_empty() {
        return Err(FlowSuggestError {
            message: "goal must not be blank".into(),
        });
    }
    if input.readme.trim().is_empty() {
        return Err(FlowSuggestError {
            message: "readme must not be blank".into(),
        });
    }

    let artifact = input
        .artifact
        .as_deref()
        .map(|value| slug_ascii_id(value, "result"))
        .unwrap_or_else(|| "result".into());
    let raw_nodes = if input.nodes.is_empty() {
        vec!["root".to_string()]
    } else {
        input.nodes
    };
    let node_ids = unique_ascii_ids(&raw_nodes, "node");
    let goal_separator = if matches!(goal.chars().last(), Some('.') | Some('!') | Some('?')) {
        ""
    } else {
        "."
    };

    let nodes = node_ids
        .iter()
        .map(|node_id| FlowNode {
            id: node_id.clone(),
            contract_id: Some(format!("contract.{node_id}")),
            action: Some(NodeAction {
                driver: NodeDriver::Agent,
                prompt_ref: Some(format!("prompts/{node_id}.md")),
                resource_refs: vec!["README.md".into()],
                reads: Vec::new(),
                writes: vec![format!("artifact.{artifact}")],
                verdict_artifact: None,
            }),
            write_scopes: Vec::new(),
            extensions: Vec::new(),
        })
        .collect::<Vec<_>>();
    let contracts = node_ids
        .iter()
        .map(|node_id| FlowContract {
            id: format!("contract.{node_id}"),
            completion: Some(ContractCompletion::AllArtifacts),
            artifacts: vec![ContractArtifact {
                id: artifact.clone(),
                schema_resource_id: Some(format!("schemas/{node_id}/{artifact}.txt")),
            }],
        })
        .collect::<Vec<_>>();
    let mut resources = vec![FlowResource {
        id: "README.md".into(),
        kind: ResourceKind::Readme,
        source: input.readme,
    }];
    resources.extend(node_ids.iter().map(|node_id| FlowResource {
        id: format!("schemas/{node_id}/{artifact}.txt"),
        kind: ResourceKind::Schema,
        source: artifact.clone(),
    }));
    resources.extend(node_ids.iter().map(|node_id| FlowResource {
        id: format!("prompts/{node_id}.md"),
        kind: ResourceKind::Prompt,
        source: format!(
            "Run node {node_id} for goal: {goal}{goal_separator} Deliver artifact with artifact_key \"{artifact}\"."
        ),
    }));

    Ok(FlowDraft {
        nodes,
        contracts,
        routes: Vec::new(),
        resources,
        imports: Vec::new(),
        policies: FlowPolicies::default(),
        extensions: Vec::new(),
    })
}

fn unique_ascii_ids(values: &[String], fallback: &str) -> Vec<String> {
    let mut counts = std::collections::HashMap::new();
    let mut used = std::collections::HashSet::new();
    values
        .iter()
        .map(|value| {
            let base = slug_ascii_id(value, fallback);
            let mut count = counts.get(&base).copied().unwrap_or(0) + 1;

            loop {
                let candidate = if count == 1 {
                    base.clone()
                } else {
                    format!("{base}_{count}")
                };

                if used.insert(candidate.clone()) {
                    counts.insert(base, count);
                    return candidate;
                }
                count += 1;
            }
        })
        .collect()
}

fn slug_ascii_id(value: &str, fallback: &str) -> String {
    let mut slug = String::new();
    let mut last_was_separator = false;

    for character in value.trim().chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            last_was_separator = false;
        } else if !slug.is_empty() && !last_was_separator {
            slug.push('_');
            last_was_separator = true;
        }
    }

    while slug.ends_with('_') {
        slug.pop();
    }

    if slug.is_empty() {
        fallback.to_string()
    } else {
        slug
    }
}
