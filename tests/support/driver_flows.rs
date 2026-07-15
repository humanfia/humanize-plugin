use std::path::Path;

use humanize_plugin::flow::{
    self, ContractArtifact, ContractCompletion, FlowCheckMode, FlowContract, FlowDraft, FlowImport,
    FlowNode, FlowPolicies, FlowPredicate, FlowQosIntent, FlowResource, FlowRoute, NetworkAccess,
    NodeAction, NodeDriver, QosUrgency, ResourceKind, ToolExecution, WorkIntent, WorkProfile,
    WorkspaceAccess, WriteScope,
};
use humanize_plugin::review::{ReviewDecision, ReviewStatus, ReviewStore};
use serde_json::Value;

pub fn approved_review_id(review_root: &Path, package: &Value) -> String {
    let lock = serde_json::from_value::<flow::FlowLock>(package.clone()).unwrap();
    let store = ReviewStore::new(review_root.to_path_buf());
    let review = store
        .prepare(
            &lock,
            &serde_json::json!({"title": "Driver fixture review"}),
            "<title>Driver fixture review</title>\n",
        )
        .unwrap();
    match review.status() {
        ReviewStatus::Pending => store
            .decide(review.review_id(), ReviewDecision::Approved, None)
            .unwrap()
            .review_id()
            .to_string(),
        ReviewStatus::Approved | ReviewStatus::Bypassed => review.review_id().to_string(),
        ReviewStatus::Rejected => panic!("driver fixture review is rejected"),
    }
}

pub fn locked_flow() -> Value {
    routed_locked_flow()
}

pub fn reviewed_lock_package() -> Value {
    let mut root = FlowNode {
        id: "root".into(),
        contract_id: Some("contract.root".into()),
        action: Some(NodeAction {
            driver: NodeDriver::Agent,
            prompt_ref: Some("prompt.root".into()),
            resource_refs: vec!["README.md".into(), "rule.safety".into()],
            reads: vec!["event.start".into()],
            writes: vec!["artifact.brief".into(), "board.summary".into()],
            verdict_artifact: Some("artifact.root_verdict".into()),
        }),
        write_scopes: vec![WriteScope::Artifact("brief".into())],
        extensions: Vec::new(),
    };
    flow::set_flow_node_work_profile(
        &mut root,
        WorkProfile {
            intent: WorkIntent::Explore,
            workspace_access: WorkspaceAccess::ReadOnly,
            tool_execution: ToolExecution::Allowed,
            network_access: NetworkAccess::Restricted,
        },
    );
    let follow = FlowNode {
        id: "follow".into(),
        contract_id: Some("contract.follow".into()),
        action: Some(NodeAction {
            driver: NodeDriver::Review,
            prompt_ref: Some("prompt.follow".into()),
            resource_refs: vec!["view.review".into()],
            reads: vec!["artifact.brief".into()],
            writes: vec!["artifact.review".into()],
            verdict_artifact: Some("artifact.review_verdict".into()),
        }),
        write_scopes: vec![WriteScope::Artifact("review".into())],
        extensions: Vec::new(),
    };
    let mut draft = FlowDraft {
        nodes: vec![root, follow],
        contracts: vec![
            FlowContract {
                id: "contract.root".into(),
                completion: Some(ContractCompletion::AllArtifacts),
                artifacts: vec![ContractArtifact {
                    id: "brief".into(),
                    schema_resource_id: Some("schema.brief".into()),
                }],
            },
            FlowContract {
                id: "contract.follow".into(),
                completion: Some(ContractCompletion::AllArtifacts),
                artifacts: vec![ContractArtifact {
                    id: "review".into(),
                    schema_resource_id: Some("schema.review".into()),
                }],
            },
        ],
        routes: vec![FlowRoute {
            predicate: FlowPredicate::exists_artifact("brief").unwrap(),
            for_each: None,
            activate: "follow".into(),
        }],
        resources: vec![
            FlowResource {
                id: "README.md".into(),
                kind: ResourceKind::Readme,
                source: "Exact reviewed lock package fixture.".into(),
            },
            FlowResource {
                id: "rule.safety".into(),
                kind: ResourceKind::Rule,
                source: "Do only the reviewed work.".into(),
            },
            FlowResource {
                id: "view.review".into(),
                kind: ResourceKind::View,
                source: "Review the brief and produce a verdict.".into(),
            },
            FlowResource {
                id: "prompt.root".into(),
                kind: ResourceKind::Prompt,
                source: "Create the brief.".into(),
            },
            FlowResource {
                id: "prompt.follow".into(),
                kind: ResourceKind::Prompt,
                source: "Review the brief.".into(),
            },
            FlowResource {
                id: "schema.brief".into(),
                kind: ResourceKind::Schema,
                source: "{\"type\":\"object\"}".into(),
            },
            FlowResource {
                id: "schema.review".into(),
                kind: ResourceKind::Schema,
                source: "{\"type\":\"object\"}".into(),
            },
        ],
        imports: vec![FlowImport {
            resource_id: "prompt.root".into(),
            alias: Some("root_prompt".into()),
        }],
        policies: FlowPolicies {
            write_scopes: vec![
                WriteScope::Artifact("brief".into()),
                WriteScope::Artifact("review".into()),
            ],
        },
        extensions: Vec::new(),
    };
    flow::set_flow_draft_qos(
        &mut draft,
        FlowQosIntent {
            urgency: QosUrgency::Interactive,
            completion_target: Some("artifact.review".into()),
        },
    );
    flow::set_flow_draft_contract_effects(
        &mut draft,
        "contract.follow",
        vec![flow::EffectRequirement {
            id: "review-notified".into(),
            required: true,
        }],
    );
    lock_package_from_draft(&draft)
}

pub fn parallel_agent_flow() -> Value {
    lock_package_from_draft(&FlowDraft {
        nodes: vec![
            FlowNode {
                id: "agent-a".into(),
                action: Some(NodeAction {
                    driver: NodeDriver::Agent,
                    prompt_ref: Some("prompt.agent-a".into()),
                    resource_refs: Vec::new(),
                    reads: Vec::new(),
                    writes: Vec::new(),
                    verdict_artifact: None,
                }),
                ..FlowNode::default()
            },
            FlowNode {
                id: "agent-b".into(),
                action: Some(NodeAction {
                    driver: NodeDriver::Agent,
                    prompt_ref: Some("prompt.agent-b".into()),
                    resource_refs: Vec::new(),
                    reads: Vec::new(),
                    writes: Vec::new(),
                    verdict_artifact: None,
                }),
                ..FlowNode::default()
            },
        ],
        resources: vec![
            FlowResource {
                id: "README.md".into(),
                kind: ResourceKind::Readme,
                source: "Parallel participant cleanup fixture.".into(),
            },
            FlowResource {
                id: "prompt.agent-a".into(),
                kind: ResourceKind::Prompt,
                source: "Handle participant A.".into(),
            },
            FlowResource {
                id: "prompt.agent-b".into(),
                kind: ResourceKind::Prompt,
                source: "Handle participant B.".into(),
            },
        ],
        ..FlowDraft::default()
    })
}

pub fn routed_locked_flow() -> Value {
    lock_package_from_draft(&FlowDraft {
        nodes: vec![
            FlowNode {
                id: "root".into(),
                contract_id: Some("contract.root".into()),
                action: Some(NodeAction {
                    driver: NodeDriver::Human,
                    prompt_ref: None,
                    resource_refs: Vec::new(),
                    reads: Vec::new(),
                    writes: vec!["artifact.brief".into()],
                    verdict_artifact: None,
                }),
                write_scopes: Vec::new(),
                extensions: Vec::new(),
            },
            FlowNode {
                id: "follow".into(),
                action: Some(NodeAction {
                    driver: NodeDriver::Human,
                    prompt_ref: None,
                    resource_refs: Vec::new(),
                    reads: vec!["artifact.brief".into()],
                    writes: Vec::new(),
                    verdict_artifact: None,
                }),
                ..FlowNode::default()
            },
        ],
        contracts: vec![FlowContract {
            id: "contract.root".into(),
            completion: Some(ContractCompletion::AllArtifacts),
            artifacts: vec![ContractArtifact {
                id: "brief".into(),
                schema_resource_id: Some("schema.root.brief".into()),
            }],
        }],
        routes: vec![FlowRoute {
            predicate: FlowPredicate::exists_artifact("brief").unwrap(),
            for_each: None,
            activate: "follow".into(),
        }],
        resources: vec![
            FlowResource {
                id: "README.md".into(),
                kind: ResourceKind::Readme,
                source: "Runtime driver locked flow.".into(),
            },
            FlowResource {
                id: "schema.root.brief".into(),
                kind: ResourceKind::Schema,
                source: "brief".into(),
            },
        ],
        imports: Vec::new(),
        policies: FlowPolicies::default(),
        extensions: Vec::new(),
    })
}

pub fn later_agent_flow() -> Value {
    lock_package_from_draft(&FlowDraft {
        nodes: vec![
            FlowNode {
                id: "root".into(),
                contract_id: Some("contract.root".into()),
                action: Some(NodeAction {
                    driver: NodeDriver::Human,
                    prompt_ref: None,
                    resource_refs: Vec::new(),
                    reads: Vec::new(),
                    writes: vec!["artifact.ready".into()],
                    verdict_artifact: None,
                }),
                write_scopes: Vec::new(),
                extensions: Vec::new(),
            },
            FlowNode {
                id: "manual".into(),
                action: Some(NodeAction {
                    driver: NodeDriver::Agent,
                    prompt_ref: Some("prompt.manual".into()),
                    resource_refs: vec!["README.md".into()],
                    reads: Vec::new(),
                    writes: Vec::new(),
                    verdict_artifact: None,
                }),
                ..FlowNode::default()
            },
        ],
        contracts: vec![FlowContract {
            id: "contract.root".into(),
            completion: Some(ContractCompletion::AllArtifacts),
            artifacts: vec![ContractArtifact {
                id: "ready".into(),
                schema_resource_id: Some("schema.ready".into()),
            }],
        }],
        routes: vec![FlowRoute {
            predicate: FlowPredicate::exists_artifact("ready").unwrap(),
            for_each: None,
            activate: "manual".into(),
        }],
        resources: vec![
            FlowResource {
                id: "README.md".into(),
                kind: ResourceKind::Readme,
                source: "Manual node context.".into(),
            },
            FlowResource {
                id: "prompt.manual".into(),
                kind: ResourceKind::Prompt,
                source: "Inspect the manual node.".into(),
            },
            FlowResource {
                id: "schema.ready".into(),
                kind: ResourceKind::Schema,
                source: "ready".into(),
            },
        ],
        imports: Vec::new(),
        policies: FlowPolicies::default(),
        extensions: Vec::new(),
    })
}

pub fn board_routed_agent_flow() -> Value {
    lock_package_from_draft(&FlowDraft {
        nodes: vec![
            FlowNode {
                id: "root".into(),
                action: Some(NodeAction {
                    driver: NodeDriver::Human,
                    prompt_ref: None,
                    resource_refs: Vec::new(),
                    reads: Vec::new(),
                    writes: vec!["board.ready".into()],
                    verdict_artifact: None,
                }),
                ..FlowNode::default()
            },
            FlowNode {
                id: "follow".into(),
                action: Some(NodeAction {
                    driver: NodeDriver::Agent,
                    prompt_ref: Some("prompt.follow".into()),
                    resource_refs: vec!["README.md".into()],
                    reads: vec!["board.ready".into()],
                    writes: Vec::new(),
                    verdict_artifact: None,
                }),
                ..FlowNode::default()
            },
        ],
        routes: vec![FlowRoute {
            predicate: FlowPredicate::exists_board("ready").unwrap(),
            for_each: None,
            activate: "follow".into(),
        }],
        resources: vec![
            FlowResource {
                id: "README.md".into(),
                kind: ResourceKind::Readme,
                source: "Board reconciliation fixture.".into(),
            },
            FlowResource {
                id: "prompt.follow".into(),
                kind: ResourceKind::Prompt,
                source: "Handle the board fact.".into(),
            },
        ],
        ..FlowDraft::default()
    })
}

pub fn quiescent_locked_flow() -> Value {
    lock_package_from_draft(&FlowDraft {
        nodes: vec![FlowNode {
            id: "root".into(),
            action: Some(NodeAction {
                driver: NodeDriver::Human,
                prompt_ref: None,
                resource_refs: Vec::new(),
                reads: Vec::new(),
                writes: vec!["artifact.items".into()],
                verdict_artifact: None,
            }),
            ..FlowNode::default()
        }],
        resources: vec![FlowResource {
            id: "README.md".into(),
            kind: ResourceKind::Readme,
            source: "Quiescent explicit scheduling fixture.".into(),
        }],
        ..FlowDraft::default()
    })
}

fn lock_package_from_draft(draft: &FlowDraft) -> Value {
    let lock = flow::flow_lock(draft, FlowCheckMode::Core).unwrap();
    serde_json::to_value(lock).unwrap()
}
