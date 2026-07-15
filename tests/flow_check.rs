use humanize_plugin::flow::{
    AdapterCapability, ArtifactRef, ArtifactRequirement, ContractArtifact, ContractCompletion,
    Diagnostic, DiagnosticDomain, EffectRequirement, FlowCheckMode, FlowContract, FlowDraft,
    FlowExportFormat, FlowImport, FlowLock, FlowNode, FlowPolicies, FlowPredicate, FlowRepairInput,
    FlowRepairReport, FlowResource, FlowRoute, FlowSuggestInput, NodeAction, NodeContract,
    NodeDriver, RepairKind, Repairability, ResourceKind, RunCompatibility, Severity, StopGate,
    WriteScope, effective_node_write_scopes, flow_check, flow_check_run_compatibility, flow_export,
    flow_lock, flow_repair, flow_suggest,
};

fn valid_draft() -> FlowDraft {
    FlowDraft {
        nodes: vec![
            FlowNode {
                id: "start".into(),
                contract_id: Some("contract.start".into()),
                ..FlowNode::default()
            },
            FlowNode {
                id: "finish".into(),
                contract_id: Some("contract.finish".into()),
                ..FlowNode::default()
            },
        ],
        contracts: vec![
            FlowContract {
                id: "contract.start".into(),
                completion: Some(ContractCompletion::Manual),
                artifacts: vec![ContractArtifact {
                    id: "handoff".into(),
                    schema_resource_id: Some("schema.handoff".into()),
                }],
            },
            FlowContract {
                id: "contract.finish".into(),
                completion: Some(ContractCompletion::AllArtifacts),
                artifacts: vec![ContractArtifact {
                    id: "summary".into(),
                    schema_resource_id: Some("schema.summary".into()),
                }],
            },
        ],
        routes: vec![FlowRoute {
            predicate: FlowPredicate::exists_artifact("handoff").unwrap(),
            for_each: None,
            activate: "finish".into(),
        }],
        resources: vec![
            FlowResource {
                id: "README.md".into(),
                kind: ResourceKind::Readme,
                source: "inline:Audit this library without editing files.".into(),
            },
            FlowResource {
                id: "schema.handoff".into(),
                kind: ResourceKind::Schema,
                source: "inline:handoff".into(),
            },
            FlowResource {
                id: "schema.summary".into(),
                kind: ResourceKind::Schema,
                source: "inline:summary".into(),
            },
        ],
        imports: vec![FlowImport {
            resource_id: "schema.handoff".into(),
            alias: Some("handoff".into()),
        }],
        policies: FlowPolicies::default(),
        extensions: Vec::new(),
    }
}

fn draft_with_ordered_authoring_data() -> FlowDraft {
    let mut draft = valid_draft();
    draft.nodes[0].action = Some(NodeAction {
        driver: NodeDriver::Agent,
        prompt_ref: Some("prompt.start".into()),
        resource_refs: vec!["schema.summary".into(), "prompt.start".into()],
        reads: vec!["artifact.input".into(), "board.state".into()],
        writes: vec!["artifact.handoff".into(), "event.started".into()],
        verdict_artifact: Some("artifact.verdict".into()),
    });
    draft.nodes[0].write_scopes = vec![
        WriteScope::Resource("schema.summary".into()),
        WriteScope::Artifact("handoff".into()),
    ];
    draft.nodes[0].extensions = vec!["Route".into(), "Node".into()];
    draft.routes = vec![
        FlowRoute {
            predicate: FlowPredicate::exists_artifact("handoff").unwrap(),
            for_each: None,
            activate: "finish".into(),
        },
        FlowRoute {
            predicate: FlowPredicate::exists_artifact("input").unwrap(),
            for_each: Some(ArtifactRef::new("items").unwrap()),
            activate: "start".into(),
        },
    ];
    draft.resources.push(FlowResource {
        id: "prompt.start".into(),
        kind: ResourceKind::Prompt,
        source: "inline:Review the handoff.".into(),
    });
    draft.imports.push(FlowImport {
        resource_id: "schema.summary".into(),
        alias: Some("summary_schema".into()),
    });
    draft.policies.write_scopes = vec![
        WriteScope::Resource("schema.summary".into()),
        WriteScope::Artifact("handoff".into()),
    ];
    draft.extensions = vec!["Route".into(), "Node".into()];
    draft
}

fn reverse_normalized_authoring_order(draft: &mut FlowDraft) {
    draft.nodes.reverse();
    draft.contracts.reverse();
    draft.routes.reverse();
    draft.resources.reverse();
    draft.imports.reverse();
    draft.policies.write_scopes.reverse();
    draft.extensions.reverse();

    if let Some(action) = &mut draft.nodes[1].action {
        action.resource_refs.reverse();
        action.reads.reverse();
        action.writes.reverse();
    }
    draft.nodes[1].write_scopes.reverse();
    draft.nodes[1].extensions.reverse();
}

fn diagnostic_codes(diagnostics: &[Diagnostic]) -> Vec<&str> {
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect()
}

fn diagnostic_domains(diagnostics: &[Diagnostic]) -> Vec<DiagnosticDomain> {
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.domain)
        .collect()
}

fn canonical_text(lock: &FlowLock) -> &str {
    std::str::from_utf8(lock.canonical_bytes()).unwrap()
}

#[test]
fn flow_suggest_builds_default_valid_skeleton() {
    let draft = flow_suggest(FlowSuggestInput {
        goal: "Summarize release risk.".into(),
        readme: "Summarize release risk without changing unrelated files.".into(),
        ..FlowSuggestInput::default()
    })
    .expect("suggested flow should be built");

    assert_eq!(
        draft.nodes,
        vec![FlowNode {
            id: "root".into(),
            contract_id: Some("contract.root".into()),
            action: Some(NodeAction {
                driver: NodeDriver::Agent,
                prompt_ref: Some("prompts/root.md".into()),
                resource_refs: vec!["README.md".into()],
                reads: Vec::new(),
                writes: vec!["artifact.result".into()],
                verdict_artifact: None,
            }),
            write_scopes: Vec::new(),
            extensions: Vec::new(),
        }]
    );
    assert_eq!(
        draft.contracts,
        vec![FlowContract {
            id: "contract.root".into(),
            completion: Some(ContractCompletion::AllArtifacts),
            artifacts: vec![ContractArtifact {
                id: "result".into(),
                schema_resource_id: Some("schemas/root/result.txt".into()),
            }],
        }]
    );
    assert_eq!(
        draft.resources,
        vec![
            FlowResource {
                id: "README.md".into(),
                kind: ResourceKind::Readme,
                source: "Summarize release risk without changing unrelated files.".into(),
            },
            FlowResource {
                id: "schemas/root/result.txt".into(),
                kind: ResourceKind::Schema,
                source: "result".into(),
            },
            FlowResource {
                id: "prompts/root.md".into(),
                kind: ResourceKind::Prompt,
                source:
                    "Run node root for goal: Summarize release risk. Deliver artifact with artifact_key \"result\"."
                        .into(),
            },
        ]
    );
    assert_eq!(draft.routes, Vec::<FlowRoute>::new());
    assert_eq!(draft.imports, Vec::<FlowImport>::new());
    assert_eq!(draft.policies, FlowPolicies::default());
    assert_eq!(draft.extensions, Vec::<String>::new());
    assert_eq!(
        draft.nodes[0].action,
        Some(NodeAction {
            driver: NodeDriver::Agent,
            prompt_ref: Some("prompts/root.md".into()),
            resource_refs: vec!["README.md".into()],
            reads: Vec::new(),
            writes: vec!["artifact.result".into()],
            verdict_artifact: None,
        })
    );
    assert_eq!(
        draft.resources[2],
        FlowResource {
            id: "prompts/root.md".into(),
            kind: ResourceKind::Prompt,
            source:
                "Run node root for goal: Summarize release risk. Deliver artifact with artifact_key \"result\"."
                    .into(),
        }
    );
    assert_eq!(
        flow_check(&draft, FlowCheckMode::Core).diagnostics,
        Vec::new()
    );
}

#[test]
fn action_descriptor_is_valid_when_prompt_resource_refs_and_fact_paths_exist() {
    let mut draft = valid_draft();
    draft.resources.extend([
        FlowResource {
            id: "prompt.review".into(),
            kind: ResourceKind::Prompt,
            source: "inline:Review the facts.".into(),
        },
        FlowResource {
            id: "script.collect".into(),
            kind: ResourceKind::Script,
            source: "scripts/collect.sh".into(),
        },
    ]);
    draft.nodes[0].action = Some(NodeAction {
        driver: NodeDriver::Review,
        prompt_ref: Some("prompt.review".into()),
        resource_refs: vec!["script.collect".into()],
        reads: vec![
            "artifact.handoff".into(),
            "board.ready".into(),
            "event.requested".into(),
        ],
        writes: vec!["artifact.summary".into(), "board.done".into()],
        verdict_artifact: Some("artifact.review_verdict".into()),
    });

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(report.diagnostics, Vec::new());
}

#[test]
fn flow_check_rejects_script_action_driver_before_lock() {
    let mut draft = valid_draft();
    draft.resources.push(FlowResource {
        id: "script.freeze".into(),
        kind: ResourceKind::Script,
        source: "scripts/freeze_task.sh".into(),
    });
    draft.nodes[0].id = "freeze_task".into();
    draft.nodes[0].action = Some(NodeAction {
        driver: NodeDriver::Script,
        prompt_ref: None,
        resource_refs: vec!["script.freeze".into()],
        reads: Vec::new(),
        writes: vec!["artifact.handoff".into()],
        verdict_artifact: None,
    });

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec!["FLOW_UNSUPPORTED_SCRIPT_ACTION_DRIVER"]
    );
    assert_eq!(
        report.diagnostics[0].domain,
        DiagnosticDomain::RuntimeCompat
    );
    assert_eq!(report.diagnostics[0].severity, Severity::Error);
    assert_eq!(
        report.diagnostics[0].location,
        "nodes[freeze_task].action.driver"
    );

    let err = flow_lock(&draft, FlowCheckMode::Core).unwrap_err();
    assert_eq!(
        diagnostic_codes(&err.diagnostics),
        vec!["FLOW_UNSUPPORTED_SCRIPT_ACTION_DRIVER"]
    );
}

#[test]
fn action_resource_refs_must_reference_known_resource_ids() {
    let mut draft = valid_draft();
    draft.nodes[0].action = Some(NodeAction {
        driver: NodeDriver::Agent,
        prompt_ref: None,
        resource_refs: vec!["script.missing".into()],
        reads: vec!["artifact.handoff".into()],
        writes: vec!["artifact.summary".into()],
        verdict_artifact: None,
    });

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec!["FLOW_UNKNOWN_ACTION_RESOURCE"]
    );
    assert_eq!(
        report.diagnostics[0].location,
        "nodes[start].action.resource_refs[0]"
    );
}

#[test]
fn action_prompt_ref_must_reference_known_resource_id() {
    let mut draft = valid_draft();
    draft.nodes[0].action = Some(NodeAction {
        driver: NodeDriver::Agent,
        prompt_ref: Some("prompt.missing".into()),
        resource_refs: Vec::new(),
        reads: vec!["artifact.handoff".into()],
        writes: vec!["artifact.summary".into()],
        verdict_artifact: None,
    });

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec!["FLOW_UNKNOWN_ACTION_PROMPT"]
    );
    assert_eq!(
        report.diagnostics[0].location,
        "nodes[start].action.prompt_ref"
    );
}

#[test]
fn action_prompt_ref_must_reference_prompt_resource_kind() {
    let mut draft = valid_draft();
    draft.resources.push(FlowResource {
        id: "script.collect".into(),
        kind: ResourceKind::Script,
        source: "scripts/collect.sh".into(),
    });
    draft.nodes[0].action = Some(NodeAction {
        driver: NodeDriver::Agent,
        prompt_ref: Some("script.collect".into()),
        resource_refs: Vec::new(),
        reads: vec!["artifact.handoff".into()],
        writes: vec!["artifact.summary".into()],
        verdict_artifact: None,
    });

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec!["FLOW_INVALID_ACTION_PROMPT"]
    );
    assert_eq!(
        report.diagnostics[0].location,
        "nodes[start].action.prompt_ref"
    );
}

#[test]
fn action_empty_verdict_artifact_is_rejected() {
    let mut draft = valid_draft();
    draft.nodes[0].action = Some(NodeAction {
        driver: NodeDriver::Human,
        prompt_ref: None,
        resource_refs: Vec::new(),
        reads: vec!["user.name".into()],
        writes: vec!["resource.output".into()],
        verdict_artifact: Some(" ".into()),
    });

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec!["FLOW_EMPTY_ACTION_VERDICT_ARTIFACT"]
    );
    assert_eq!(
        report.diagnostics[0].location,
        "nodes[start].action.verdict_artifact"
    );
}

#[test]
fn flow_suggest_slugs_and_deduplicates_node_ids() {
    let draft = flow_suggest(FlowSuggestInput {
        goal: "Build a compact migration brief.".into(),
        readme: "Build a compact migration brief.".into(),
        nodes: vec![
            " Review API ".into(),
            "review_api_2".into(),
            "review_api".into(),
            format!("D{}j{} Vu", '\u{00e9}', '\u{00e0}'),
            "!!!".into(),
            " ".into(),
        ],
        artifact: Some(" !!! ".into()),
    })
    .expect("suggested flow should be built");

    let node_ids = draft
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        node_ids,
        vec![
            "review_api",
            "review_api_2",
            "review_api_3",
            "d_j_vu",
            "node",
            "node_2"
        ]
    );
    assert_eq!(
        draft
            .nodes
            .iter()
            .map(|node| node.contract_id.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("contract.review_api"),
            Some("contract.review_api_2"),
            Some("contract.review_api_3"),
            Some("contract.d_j_vu"),
            Some("contract.node"),
            Some("contract.node_2"),
        ]
    );
    assert_eq!(
        draft
            .contracts
            .iter()
            .map(|contract| {
                (
                    contract.id.as_str(),
                    contract.completion.as_ref(),
                    contract.artifacts[0].id.as_str(),
                    contract.artifacts[0].schema_resource_id.as_deref(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                "contract.review_api",
                Some(&ContractCompletion::AllArtifacts),
                "result",
                Some("schemas/review_api/result.txt"),
            ),
            (
                "contract.review_api_2",
                Some(&ContractCompletion::AllArtifacts),
                "result",
                Some("schemas/review_api_2/result.txt"),
            ),
            (
                "contract.review_api_3",
                Some(&ContractCompletion::AllArtifacts),
                "result",
                Some("schemas/review_api_3/result.txt"),
            ),
            (
                "contract.d_j_vu",
                Some(&ContractCompletion::AllArtifacts),
                "result",
                Some("schemas/d_j_vu/result.txt"),
            ),
            (
                "contract.node",
                Some(&ContractCompletion::AllArtifacts),
                "result",
                Some("schemas/node/result.txt"),
            ),
            (
                "contract.node_2",
                Some(&ContractCompletion::AllArtifacts),
                "result",
                Some("schemas/node_2/result.txt"),
            ),
        ]
    );
    assert_eq!(
        draft
            .resources
            .iter()
            .filter(|resource| resource.kind == ResourceKind::Schema)
            .map(|resource| (resource.id.as_str(), resource.source.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("schemas/review_api/result.txt", "result"),
            ("schemas/review_api_2/result.txt", "result"),
            ("schemas/review_api_3/result.txt", "result"),
            ("schemas/d_j_vu/result.txt", "result"),
            ("schemas/node/result.txt", "result"),
            ("schemas/node_2/result.txt", "result"),
        ]
    );
    assert_eq!(
        flow_check(&draft, FlowCheckMode::Core).diagnostics,
        Vec::new()
    );
}

#[test]
fn core_check_reports_authoring_errors() {
    let mut draft = valid_draft();
    draft.routes.push(FlowRoute {
        predicate: FlowPredicate::exists_artifact("ready").unwrap(),
        for_each: None,
        activate: "missing-node".into(),
    });
    draft.contracts.push(FlowContract {
        id: "contract.incomplete".into(),
        completion: None,
        artifacts: vec![ContractArtifact {
            id: "artifact-without-schema".into(),
            schema_resource_id: None,
        }],
    });
    draft.extensions = vec!["Effect".into(), "NodeActivation".into()];

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(report.mode, FlowCheckMode::Core);
    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec![
            "FLOW_UNKNOWN_ROUTE_TARGET",
            "FLOW_MISSING_CONTRACT_COMPLETION",
            "FLOW_MISSING_ARTIFACT_SCHEMA",
            "FLOW_AUTHORING_PRIMITIVE_MISUSE",
            "FLOW_AUTHORING_PRIMITIVE_MISUSE",
        ]
    );
    assert_eq!(
        report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.severity)
            .collect::<Vec<_>>(),
        vec![
            Severity::Error,
            Severity::Error,
            Severity::Error,
            Severity::Fatal,
            Severity::Fatal,
        ]
    );
    assert!(report.diagnostics.iter().all(|diagnostic| {
        !diagnostic.location.is_empty()
            && !diagnostic.message.is_empty()
            && diagnostic.fix_hint.is_some()
            && diagnostic.why_it_matters.is_some()
    }));
}

#[test]
fn diagnostics_include_stable_metadata_fields() {
    let mut draft = valid_draft();
    draft.resources[0].source = " ".into();
    draft.routes[0].activate = "missing-node".into();
    draft.contracts[0].artifacts[0].schema_resource_id = Some("schema.missing".into());
    draft.policies.write_scopes = vec![WriteScope::Workspace];

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec![
            "FLOW_EMPTY_README",
            "FLOW_UNKNOWN_ROUTE_TARGET",
            "FLOW_MISSING_ARTIFACT_SCHEMA",
            "FLOW_BROAD_WRITE_SCOPE",
        ]
    );
    assert_eq!(
        diagnostic_domains(&report.diagnostics),
        vec![
            DiagnosticDomain::Package,
            DiagnosticDomain::Route,
            DiagnosticDomain::Resource,
            DiagnosticDomain::Policy,
        ]
    );
    assert_eq!(
        report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.severity)
            .collect::<Vec<_>>(),
        vec![
            Severity::Error,
            Severity::Error,
            Severity::Error,
            Severity::Warning,
        ]
    );
    assert_eq!(
        report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.repairability)
            .collect::<Vec<_>>(),
        vec![
            Repairability::GuidanceOnly,
            Repairability::Candidate,
            Repairability::GuidanceOnly,
            Repairability::GuidanceOnly,
        ]
    );
    assert!(
        report
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.why_it_matters.is_some())
    );
    assert!(
        report
            .diagnostics
            .iter()
            .flat_map(|diagnostic| diagnostic.repair_kinds.iter())
            .any(|kind| *kind == RepairKind::AddRouteTarget)
    );
}

#[test]
fn diagnostics_cover_contract_and_runtime_compat_domains() {
    let mut draft = valid_draft();
    draft.contracts[0].completion = None;

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_domains(&report.diagnostics),
        vec![DiagnosticDomain::Contract]
    );
    assert_eq!(
        report.diagnostics[0].repairability,
        Repairability::GuidanceOnly
    );

    let result = flow_check_run_compatibility(
        &valid_draft(),
        RunCompatibility {
            available_resources: vec!["README.md".into(), "schema.handoff".into()],
        },
    );

    assert_eq!(
        diagnostic_domains(&result.diagnostics),
        vec![DiagnosticDomain::RuntimeCompat]
    );
    assert_eq!(
        result.diagnostics[0].repairability,
        Repairability::GuidanceOnly
    );
}

#[test]
fn readme_diagnostics_are_guidance_only_errors() {
    let mut missing = valid_draft();
    missing
        .resources
        .retain(|resource| resource.id != "README.md");
    let missing_report = flow_check(&missing, FlowCheckMode::Core);

    assert_eq!(
        missing_report.diagnostics[0].domain,
        DiagnosticDomain::Package
    );
    assert_eq!(missing_report.diagnostics[0].severity, Severity::Error);
    assert_eq!(
        missing_report.diagnostics[0].repairability,
        Repairability::GuidanceOnly
    );
    assert_eq!(
        missing_report.diagnostics[0].repair_kinds,
        Vec::<RepairKind>::new()
    );

    let mut empty = valid_draft();
    empty.resources[0].source = " ".into();
    let repair = flow_repair(&FlowRepairInput::from_draft(empty, FlowCheckMode::Core));

    assert_eq!(
        diagnostic_codes(&repair.diagnostics),
        vec!["FLOW_EMPTY_README"]
    );
    assert!(repair.candidates.is_empty());

    let missing_repair = flow_repair(&FlowRepairInput::from_draft(missing, FlowCheckMode::Core));
    assert_eq!(
        diagnostic_codes(&missing_repair.diagnostics),
        vec!["FLOW_MISSING_README"]
    );
    assert!(missing_repair.candidates.is_empty());
}

#[test]
fn node_contract_model_projects_existing_draft_fields() {
    let mut draft = valid_draft();
    draft.nodes[0].action = Some(NodeAction {
        driver: NodeDriver::Agent,
        prompt_ref: Some("prompt.start".into()),
        resource_refs: vec!["schema.handoff".into()],
        reads: vec!["artifact.input".into()],
        writes: vec!["artifact.handoff".into()],
        verdict_artifact: Some("artifact.verdict".into()),
    });
    draft.resources.push(FlowResource {
        id: "prompt.start".into(),
        kind: ResourceKind::Prompt,
        source: "inline:Collect the handoff.".into(),
    });

    let contracts = NodeContract::from_draft(&draft);
    let capability = AdapterCapability::from_action(
        "start",
        draft.nodes[0].action.as_ref().expect("action should exist"),
    );

    assert_eq!(contracts[0].node_id, "start");
    assert_eq!(contracts[0].contract_id.as_deref(), Some("contract.start"));
    assert_eq!(contracts[0].requires, vec!["artifact.input"]);
    assert_eq!(contracts[0].prefers, vec!["prompt.start", "schema.handoff"]);
    assert_eq!(
        contracts[0].accepts,
        vec!["artifact.handoff", "artifact.verdict"]
    );
    assert_eq!(
        contracts[0].artifact_requirements,
        vec![ArtifactRequirement {
            id: "handoff".into(),
            schema_resource_id: Some("schema.handoff".into()),
            required: true,
        }]
    );
    assert_eq!(
        contracts[0].effect_requirements,
        Vec::<EffectRequirement>::new()
    );
    assert_eq!(contracts[0].stop_gate, StopGate::Preferred);
    assert_eq!(capability.node_id, "start");
    assert_eq!(capability.requires, vec!["artifact.input"]);
    assert_eq!(
        capability.accepts,
        vec!["artifact.handoff", "artifact.verdict"]
    );
}

#[test]
fn core_check_rejects_node_contract_id_without_matching_contract() {
    let mut draft = valid_draft();
    draft.nodes[0].contract_id = Some("contract.missing".into());

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec!["FLOW_UNKNOWN_NODE_CONTRACT"]
    );
    let diagnostic = &report.diagnostics[0];
    assert_eq!(diagnostic.domain, DiagnosticDomain::Contract);
    assert_eq!(diagnostic.severity, Severity::Error);
    assert_eq!(diagnostic.severity, Severity::Error);
    assert_eq!(diagnostic.repairability, Repairability::GuidanceOnly);
    assert_eq!(diagnostic.location, "nodes[start].contract_id");
    assert!(diagnostic.message.contains("contract.missing"));
    assert_eq!(diagnostic.repair_kinds, Vec::<RepairKind>::new());

    let err = flow_lock(&draft, FlowCheckMode::Core).unwrap_err();
    assert_eq!(
        diagnostic_codes(&err.diagnostics),
        vec!["FLOW_UNKNOWN_NODE_CONTRACT"]
    );
}

#[test]
fn core_check_rejects_duplicate_node_contract_and_contract_artifact_keys() {
    let mut draft = valid_draft();
    draft.nodes.push(draft.nodes[0].clone());
    draft.contracts.push(draft.contracts[0].clone());
    let duplicate_artifact = draft.contracts[0].artifacts[0].clone();
    draft.contracts[0].artifacts.push(duplicate_artifact);

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec![
            "FLOW_DUPLICATE_NODE_ID",
            "FLOW_DUPLICATE_CONTRACT_ARTIFACT",
            "FLOW_DUPLICATE_CONTRACT_ID",
        ]
    );
    assert!(flow_lock(&draft, FlowCheckMode::Core).is_err());
}

#[test]
fn flow_repair_returns_no_candidates_for_valid_drafts() {
    let input = FlowRepairInput::from_draft(valid_draft(), FlowCheckMode::Core);

    let repair = flow_repair(&input);

    assert_eq!(repair.diagnostics, Vec::new());
    assert!(repair.candidates.is_empty());
}

#[test]
fn flow_repair_report_exposes_only_diagnostics_and_candidates() {
    let report = flow_repair(&FlowRepairInput::from_draft(
        valid_draft(),
        FlowCheckMode::Core,
    ));

    let FlowRepairReport {
        diagnostics,
        candidates,
    } = report;
    assert!(diagnostics.is_empty());
    assert!(candidates.is_empty());
}

#[test]
fn flow_repair_returns_local_candidates_for_mechanical_route_errors() {
    let mut draft = valid_draft();
    draft.nodes.truncate(1);
    draft.routes[0].activate = "missing".into();

    let repair = flow_repair(&FlowRepairInput::from_draft(draft, FlowCheckMode::Core));

    assert_eq!(
        diagnostic_codes(&repair.diagnostics),
        vec!["FLOW_UNKNOWN_ROUTE_TARGET"]
    );
    assert_eq!(repair.candidates.len(), 1);
    assert_eq!(repair.candidates[0].repair_kind, RepairKind::AddRouteTarget);
    assert_eq!(repair.candidates[0].location, "routes[0].activate");
    assert_eq!(repair.candidates[0].replacement, "start");
}

#[test]
fn flow_repair_preserves_authored_candidate_order_without_ranking() {
    let mut draft = valid_draft();
    draft.nodes = vec![
        FlowNode {
            id: "zeta".into(),
            ..FlowNode::default()
        },
        FlowNode {
            id: "alpha".into(),
            ..FlowNode::default()
        },
    ];
    draft.routes[0].activate = "missing".into();

    let repair = flow_repair(&FlowRepairInput::from_draft(draft, FlowCheckMode::Core));

    assert_eq!(
        repair
            .candidates
            .iter()
            .map(|candidate| candidate.replacement.as_str())
            .collect::<Vec<_>>(),
        vec!["zeta", "alpha"]
    );
}

#[test]
fn flow_repair_keeps_non_mechanical_errors_as_guidance() {
    let mut draft = valid_draft();
    draft.contracts[0].completion = None;

    let repair = flow_repair(&FlowRepairInput::from_draft(draft, FlowCheckMode::Core));

    assert_eq!(
        diagnostic_codes(&repair.diagnostics),
        vec!["FLOW_MISSING_CONTRACT_COMPLETION"]
    );
    assert_eq!(
        repair.diagnostics[0].repairability,
        Repairability::GuidanceOnly
    );
    assert!(repair.candidates.is_empty());
}

#[test]
fn flow_repair_includes_warnings_only_when_requested() {
    let mut draft = valid_draft();
    draft.policies.write_scopes = vec![WriteScope::Workspace];

    let default_report = flow_repair(&FlowRepairInput::from_draft(
        draft.clone(),
        FlowCheckMode::Core,
    ));
    let mut warning_input = FlowRepairInput::from_draft(draft, FlowCheckMode::Core);
    warning_input.include_warnings = true;
    let warning_report = flow_repair(&warning_input);

    assert!(default_report.diagnostics.is_empty());
    assert!(default_report.candidates.is_empty());
    assert_eq!(
        diagnostic_codes(&warning_report.diagnostics),
        vec!["FLOW_BROAD_WRITE_SCOPE"]
    );
    assert_eq!(warning_report.diagnostics[0].severity, Severity::Warning);
    assert_eq!(
        warning_report.diagnostics[0].repairability,
        Repairability::GuidanceOnly
    );
    assert!(warning_report.candidates.is_empty());
}

#[test]
fn fatal_diagnostics_suppress_repair_candidates() {
    let mut draft = valid_draft();
    draft.extensions = vec!["NodeActivation".into()];
    draft.routes[0].activate = "missing".into();
    let input = FlowRepairInput::from_draft(draft, FlowCheckMode::Core);

    let repair = flow_repair(&input);

    assert_eq!(
        diagnostic_codes(&repair.diagnostics),
        vec!["FLOW_AUTHORING_PRIMITIVE_MISUSE"]
    );
    assert!(
        repair
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Fatal)
    );
    assert!(repair.candidates.is_empty());
}

#[test]
fn core_check_requires_readme_resource_for_runnable_drafts() {
    let mut draft = valid_draft();
    draft
        .resources
        .retain(|resource| resource.id != "README.md");

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec!["FLOW_MISSING_README"]
    );
    assert_eq!(report.diagnostics[0].severity, Severity::Error);
    assert_eq!(report.diagnostics[0].location, "resources");
    assert!(report.diagnostics[0].message.contains("README"));

    let err = flow_lock(&draft, FlowCheckMode::Core).unwrap_err();
    assert_eq!(
        diagnostic_codes(&err.diagnostics),
        vec!["FLOW_MISSING_README"]
    );
}

#[test]
fn core_check_requires_readme_content_for_runnable_drafts() {
    for source in ["", "   ", "\n\t"] {
        let mut draft = valid_draft();
        draft.resources[0].source = source.into();

        let report = flow_check(&draft, FlowCheckMode::Core);

        assert_eq!(
            diagnostic_codes(&report.diagnostics),
            vec!["FLOW_EMPTY_README"],
            "source {source:?} should be rejected"
        );
        assert_eq!(report.diagnostics[0].severity, Severity::Error);
        assert_eq!(report.diagnostics[0].location, "resources");
    }
}

#[test]
fn core_check_rejects_multiple_readme_resources_when_all_are_empty() {
    let mut draft = valid_draft();
    draft.resources[0].source = " ".into();
    draft.resources.push(FlowResource {
        id: "README.md".into(),
        kind: ResourceKind::Readme,
        source: "  ".into(),
    });

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec!["FLOW_DUPLICATE_RESOURCE_PATH", "FLOW_EMPTY_README"]
    );
    assert_eq!(report.diagnostics[0].severity, Severity::Error);
}

#[test]
fn core_check_accepts_valid_root_readme_resource() {
    let mut draft = valid_draft();
    draft.resources[0].source = "Root package description.\n".into();

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(report.diagnostics, Vec::new());
}

#[test]
fn flow_lock_rejects_empty_readme() {
    let mut draft = valid_draft();
    draft.resources[0].source = " ".into();

    let err = flow_lock(&draft, FlowCheckMode::Core).unwrap_err();

    assert_eq!(
        diagnostic_codes(&err.diagnostics),
        vec!["FLOW_EMPTY_README"]
    );
}

#[test]
fn core_check_requires_readme_for_node_less_non_empty_drafts() {
    let cases = [
        (
            "resources",
            FlowDraft {
                resources: vec![FlowResource {
                    id: "schema.handoff".into(),
                    kind: ResourceKind::Schema,
                    source: "inline:handoff".into(),
                }],
                ..FlowDraft::default()
            },
        ),
        (
            "imports",
            FlowDraft {
                imports: vec![FlowImport {
                    resource_id: "schema.handoff".into(),
                    alias: Some("handoff".into()),
                }],
                ..FlowDraft::default()
            },
        ),
        (
            "contracts",
            FlowDraft {
                contracts: vec![FlowContract {
                    id: "contract.audit".into(),
                    completion: Some(ContractCompletion::Manual),
                    artifacts: Vec::new(),
                }],
                ..FlowDraft::default()
            },
        ),
        (
            "routes",
            FlowDraft {
                routes: vec![FlowRoute {
                    predicate: FlowPredicate::exists_artifact("ready").unwrap(),
                    for_each: None,
                    activate: "review".into(),
                }],
                ..FlowDraft::default()
            },
        ),
        (
            "policies",
            FlowDraft {
                policies: FlowPolicies {
                    write_scopes: vec![WriteScope::Artifact("handoff".into())],
                },
                ..FlowDraft::default()
            },
        ),
        (
            "extensions",
            FlowDraft {
                extensions: vec!["Route".into()],
                ..FlowDraft::default()
            },
        ),
    ];

    for (name, draft) in cases {
        let report = flow_check(&draft, FlowCheckMode::Core);
        let codes = diagnostic_codes(&report.diagnostics);

        assert!(
            codes.contains(&"FLOW_MISSING_README"),
            "{name} draft should require a README resource, got {codes:?}"
        );
    }
}

#[test]
fn flow_lock_rejects_node_less_resource_package_without_readme() {
    let draft = FlowDraft {
        resources: vec![FlowResource {
            id: "schema.handoff".into(),
            kind: ResourceKind::Schema,
            source: "inline:handoff".into(),
        }],
        ..FlowDraft::default()
    };

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec!["FLOW_MISSING_README"]
    );

    let err = flow_lock(&draft, FlowCheckMode::Core).unwrap_err();
    assert_eq!(
        diagnostic_codes(&err.diagnostics),
        vec!["FLOW_MISSING_README"]
    );
}

#[test]
fn broad_write_scope_is_warning_in_core_and_error_in_strict() {
    let mut draft = valid_draft();
    draft.policies.write_scopes = vec![WriteScope::Workspace];

    let core_report = flow_check(&draft, FlowCheckMode::Core);
    let strict_report = flow_check(&draft, FlowCheckMode::Strict);
    let core_lock = flow_lock(&draft, FlowCheckMode::Core).unwrap();

    assert_eq!(core_report.diagnostics.len(), 1);
    assert_eq!(core_report.diagnostics[0].code, "FLOW_BROAD_WRITE_SCOPE");
    assert_eq!(core_report.diagnostics[0].severity, Severity::Warning);
    assert!(canonical_text(&core_lock).contains("\"domain\":\"policy\""));
    assert!(canonical_text(&core_lock).contains("\"repairability\":\"guidance_only\""));

    assert_eq!(strict_report.diagnostics.len(), 1);
    assert_eq!(strict_report.diagnostics[0].code, "FLOW_BROAD_WRITE_SCOPE");
    assert_eq!(strict_report.diagnostics[0].severity, Severity::Error);
}

#[test]
fn flow_lock_refuses_drafts_with_core_errors() {
    let mut draft = valid_draft();
    draft.routes[0].activate = "missing-node".into();

    let err = flow_lock(&draft, FlowCheckMode::Core).unwrap_err();

    assert_eq!(
        diagnostic_codes(&err.diagnostics),
        vec!["FLOW_UNKNOWN_ROUTE_TARGET"]
    );
}

#[test]
fn route_predicates_and_fanout_survive_lock_normalization_and_export() {
    let mut draft = valid_draft();
    draft.routes = vec![
        FlowRoute {
            predicate: FlowPredicate::truthy_board("ready").unwrap(),
            for_each: None,
            activate: "finish".into(),
        },
        FlowRoute {
            predicate: FlowPredicate::exists_artifact("items").unwrap(),
            for_each: Some(ArtifactRef::new("items").unwrap()),
            activate: "finish".into(),
        },
    ];

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(report.diagnostics, Vec::new());

    let lock = flow_lock(&draft, FlowCheckMode::Core).unwrap();
    let canonical = canonical_text(&lock);

    assert!(canonical.contains(
        "\"predicate\":{\"op\":\"exists\",\"fact\":{\"kind\":\"artifact\",\"key\":\"items\"}}"
    ));
    assert!(canonical.contains("\"for_each\":{\"key\":\"items\"}"));
    assert!(canonical.contains("\"path\":\"README.md\""));
    assert!(canonical.contains("\"kind\":\"readme\""));
    assert!(canonical.contains("\"format\":\"humanize.flow_lock.v1\""));
    assert!(!canonical.contains("\"node_contracts\""));

    let json = flow_export(&lock, FlowExportFormat::Json);
    let yaml = flow_export(&lock, FlowExportFormat::Yaml);

    assert!(json.contains("\"key\": \"items\""));
    assert!(json.contains("README.md"));
    assert!(yaml.contains("key: \"items\""));
    assert!(yaml.contains("README.md"));
}

#[test]
fn action_descriptor_survives_lock_normalization_and_export() {
    let mut draft = valid_draft();
    draft.resources.extend([
        FlowResource {
            id: "script.collect".into(),
            kind: ResourceKind::Script,
            source: "scripts/collect.sh".into(),
        },
        FlowResource {
            id: "prompt.review".into(),
            kind: ResourceKind::Prompt,
            source: "inline:Review the facts.".into(),
        },
        FlowResource {
            id: "view.summary".into(),
            kind: ResourceKind::View,
            source: "views/summary.json".into(),
        },
    ]);
    draft.nodes[0].action = Some(NodeAction {
        driver: NodeDriver::Review,
        prompt_ref: Some("prompt.review".into()),
        resource_refs: vec!["view.summary".into(), "script.collect".into()],
        reads: vec!["event.requested".into(), "artifact.handoff".into()],
        writes: vec!["board.done".into(), "artifact.summary".into()],
        verdict_artifact: Some("artifact.review_verdict".into()),
    });

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(report.diagnostics, Vec::new());

    let lock = flow_lock(&draft, FlowCheckMode::Core).unwrap();
    let canonical = canonical_text(&lock);

    assert!(canonical.contains(
        "\"action\":{\"driver\":\"review\",\"prompt_ref\":\"prompt.review\",\"resource_refs\":[\"script.collect\",\"view.summary\"],\"reads\":[\"artifact.handoff\",\"event.requested\"],\"writes\":[\"artifact.summary\",\"board.done\"],\"verdict_artifact\":\"artifact.review_verdict\"}"
    ));

    let json = flow_export(&lock, FlowExportFormat::Json);
    let yaml = flow_export(&lock, FlowExportFormat::Yaml);

    assert!(json.contains("artifact.review_verdict"));
    assert!(json.contains("prompt.review"));
    assert!(yaml.contains("artifact.review_verdict"));
    assert!(yaml.contains("prompt.review"));
}

#[test]
fn route_activate_targets_must_exist() {
    let mut draft = valid_draft();
    draft.routes[0].activate = "missing-node".into();

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec!["FLOW_UNKNOWN_ROUTE_TARGET"]
    );
    assert_eq!(report.diagnostics[0].location, "routes[0].activate");
}

#[test]
fn effective_node_write_scopes_are_intersected_with_flow_policy() {
    let policies = FlowPolicies {
        write_scopes: vec![
            WriteScope::Artifact("handoff".into()),
            WriteScope::Resource("schema.summary".into()),
        ],
    };
    let node = FlowNode {
        id: "start".into(),
        write_scopes: vec![
            WriteScope::Artifact("handoff".into()),
            WriteScope::Workspace,
            WriteScope::Resource("schema.other".into()),
        ],
        ..FlowNode::default()
    };

    assert_eq!(
        effective_node_write_scopes(&policies, &node),
        vec![WriteScope::Artifact("handoff".into())]
    );
    assert_eq!(
        effective_node_write_scopes(&FlowPolicies::default(), &node),
        Vec::<WriteScope>::new()
    );
}

#[test]
fn extension_kinds_are_allowlisted_to_authoring_names() {
    let mut allowed = valid_draft();
    allowed.extensions = vec!["Route".into(), "Resource".into()];
    allowed.nodes[0].extensions = vec!["Contract".into()];

    assert_eq!(
        flow_check(&allowed, FlowCheckMode::Core).diagnostics,
        Vec::new()
    );

    let mut denied = valid_draft();
    denied.extensions = vec![
        "Activation".into(),
        "NodeActivation".into(),
        "Effect".into(),
        "FlowApplied".into(),
        "EffectRecorded".into(),
        "UnknownRuntimeThing".into(),
    ];

    let report = flow_check(&denied, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec![
            "FLOW_AUTHORING_PRIMITIVE_MISUSE",
            "FLOW_AUTHORING_PRIMITIVE_MISUSE",
            "FLOW_AUTHORING_PRIMITIVE_MISUSE",
            "FLOW_AUTHORING_PRIMITIVE_MISUSE",
            "FLOW_AUTHORING_PRIMITIVE_MISUSE",
            "FLOW_AUTHORING_PRIMITIVE_MISUSE",
        ]
    );
}

#[test]
fn flow_lock_id_is_deterministic_from_canonical_content_and_check_mode() {
    let draft = valid_draft();
    let mut reordered = valid_draft();
    reordered.nodes.reverse();
    reordered.contracts.reverse();
    reordered.resources.reverse();

    let core_lock = flow_lock(&draft, FlowCheckMode::Core).unwrap();
    let repeated_core_lock = flow_lock(&reordered, FlowCheckMode::Core).unwrap();
    let strict_lock = flow_lock(&draft, FlowCheckMode::Strict).unwrap();

    assert_eq!(core_lock.id(), repeated_core_lock.id());
    assert_ne!(core_lock.id(), strict_lock.id());
    assert_eq!(core_lock.mode(), FlowCheckMode::Core);
    assert_eq!(strict_lock.mode(), FlowCheckMode::Strict);
}

#[test]
fn flow_lock_stores_the_exact_canonical_draft_used_for_identity() {
    let draft = draft_with_ordered_authoring_data();

    let lock = flow_lock(&draft, FlowCheckMode::Core).unwrap();
    let relocked = flow_lock(lock.draft(), FlowCheckMode::Core).unwrap();

    assert_eq!(lock, relocked);
    assert_eq!(lock.canonical_bytes(), relocked.canonical_bytes());
    assert_eq!(
        serde_json::from_slice::<FlowLock>(&serde_json::to_vec(&lock).unwrap()).unwrap(),
        lock
    );
}

#[test]
fn reordered_input_produces_the_same_canonical_stored_lock_and_bytes() {
    let draft = draft_with_ordered_authoring_data();
    let mut reordered = draft.clone();
    reverse_normalized_authoring_order(&mut reordered);

    let lock = flow_lock(&draft, FlowCheckMode::Core).unwrap();
    let reordered_lock = flow_lock(&reordered, FlowCheckMode::Core).unwrap();

    assert_eq!(lock.id(), reordered_lock.id());
    assert_eq!(lock, reordered_lock);
    assert_eq!(lock.draft(), reordered_lock.draft());
    assert_eq!(
        serde_json::to_vec(&lock).unwrap(),
        serde_json::to_vec(&reordered_lock).unwrap()
    );
}

#[test]
fn flow_export_is_deterministic_for_json_and_yaml() {
    let lock = flow_lock(&valid_draft(), FlowCheckMode::Core).unwrap();

    let json = flow_export(&lock, FlowExportFormat::Json);
    let repeated_json = flow_export(&lock, FlowExportFormat::Json);
    let yaml = flow_export(&lock, FlowExportFormat::Yaml);
    let repeated_yaml = flow_export(&lock, FlowExportFormat::Yaml);

    assert_eq!(json, repeated_json);
    assert_eq!(yaml, repeated_yaml);
    assert!(json.starts_with("{\n"));
    assert!(json.contains(lock.id()));
    assert!(yaml.starts_with("check_mode: "));
    assert!(yaml.contains("format: "));
    assert!(yaml.contains(lock.id()));
}

#[test]
fn run_compatibility_reports_unavailable_resources() {
    let input = RunCompatibility {
        available_resources: vec!["README.md".into(), "schema.handoff".into()],
    };

    let result = flow_check_run_compatibility(&valid_draft(), input);

    assert!(!result.compatible);
    assert_eq!(
        diagnostic_codes(&result.diagnostics),
        vec!["FLOW_RUN_RESOURCE_UNAVAILABLE"]
    );
}
