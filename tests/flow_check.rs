use humanize_plugin::flow::{
    ContractArtifact, ContractCompletion, Diagnostic, FlowCheckMode, FlowContract, FlowDraft,
    FlowExportFormat, FlowImport, FlowNode, FlowPolicies, FlowResource, FlowRoute, ResourceKind,
    RunCompatibility, Severity, WriteScope, effective_node_write_scopes, flow_check,
    flow_check_run_compatibility, flow_export, flow_lock,
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
            predicate: "exists(artifact.handoff)".into(),
            for_each: None,
            activate: "finish".into(),
        }],
        resources: vec![
            FlowResource {
                id: "readme.main".into(),
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

fn diagnostic_codes(diagnostics: &[Diagnostic]) -> Vec<&str> {
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect()
}

#[test]
fn core_check_reports_authoring_errors() {
    let mut draft = valid_draft();
    draft.routes.push(FlowRoute {
        predicate: "exists(artifact.ready)".into(),
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
    assert!(report.diagnostics.iter().all(|diagnostic| {
        diagnostic.severity == Severity::Error
            && !diagnostic.location.is_empty()
            && !diagnostic.message.is_empty()
            && diagnostic.fix_hint.is_some()
    }));
}

#[test]
fn core_check_requires_readme_resource_for_runnable_drafts() {
    let mut draft = valid_draft();
    draft
        .resources
        .retain(|resource| resource.id != "readme.main");

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
                    predicate: "exists(artifact.ready)".into(),
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

    assert_eq!(core_report.diagnostics.len(), 1);
    assert_eq!(core_report.diagnostics[0].code, "FLOW_BROAD_WRITE_SCOPE");
    assert_eq!(core_report.diagnostics[0].severity, Severity::Warning);

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
            predicate: "artifact.schema == 'handoff.v1' && board.ready == true".into(),
            for_each: None,
            activate: "finish".into(),
        },
        FlowRoute {
            predicate: "exists(event.review_requested)".into(),
            for_each: Some("artifact.items".into()),
            activate: "finish".into(),
        },
    ];

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(report.diagnostics, Vec::new());

    let lock = flow_lock(&draft, FlowCheckMode::Core).unwrap();
    let normalized = lock.normalized_content();

    assert!(normalized.contains("\"predicate\":\"exists(event.review_requested)\""));
    assert!(normalized.contains("\"for_each\":\"artifact.items\""));
    assert!(normalized.contains("\"id\":\"readme.main\""));
    assert!(normalized.contains("\"kind\":\"readme\""));

    let json = flow_export(&lock, FlowExportFormat::Json);
    let yaml = flow_export(&lock, FlowExportFormat::Yaml);

    assert!(json.contains("exists(event.review_requested)"));
    assert!(json.contains("readme.main"));
    assert!(yaml.contains("artifact.items"));
    assert!(yaml.contains("readme.main"));
}

#[test]
fn route_predicates_reject_effectful_calls_and_non_fact_roots() {
    let mut draft = valid_draft();
    draft.routes = [
        "shell('cargo test')",
        "mcp('activate_node')",
        "patch_board('ready', true)",
        "deliver_artifact(artifact.handoff)",
        "activate_node('finish')",
        "send_message('done')",
        "flow_apply('child')",
        "flow_lock('child')",
        "user.is_admin == true",
        "true",
        "exists()",
    ]
    .into_iter()
    .map(|predicate| FlowRoute {
        predicate: predicate.into(),
        for_each: None,
        activate: "finish".into(),
    })
    .collect();

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec![
            "FLOW_ROUTE_PREDICATE_NOT_FACT_DRIVEN",
            "FLOW_ROUTE_PREDICATE_NOT_FACT_DRIVEN",
            "FLOW_ROUTE_PREDICATE_NOT_FACT_DRIVEN",
            "FLOW_ROUTE_PREDICATE_NOT_FACT_DRIVEN",
            "FLOW_ROUTE_PREDICATE_NOT_FACT_DRIVEN",
            "FLOW_ROUTE_PREDICATE_NOT_FACT_DRIVEN",
            "FLOW_ROUTE_PREDICATE_NOT_FACT_DRIVEN",
            "FLOW_ROUTE_PREDICATE_NOT_FACT_DRIVEN",
            "FLOW_ROUTE_PREDICATE_NOT_FACT_DRIVEN",
            "FLOW_ROUTE_PREDICATE_NOT_FACT_DRIVEN",
            "FLOW_ROUTE_PREDICATE_NOT_FACT_DRIVEN",
        ]
    );
}

#[test]
fn route_for_each_must_iterate_artifact_fact_paths() {
    let mut draft = valid_draft();
    draft.routes = vec![
        FlowRoute {
            predicate: "exists(artifact.items)".into(),
            for_each: Some("board.items".into()),
            activate: "finish".into(),
        },
        FlowRoute {
            predicate: "exists(artifact.items)".into(),
            for_each: Some("artifact.items.map(shell)".into()),
            activate: "finish".into(),
        },
    ];

    let report = flow_check(&draft, FlowCheckMode::Core);

    assert_eq!(
        diagnostic_codes(&report.diagnostics),
        vec![
            "FLOW_ROUTE_FOR_EACH_NOT_ARTIFACT_DRIVEN",
            "FLOW_ROUTE_FOR_EACH_NOT_ARTIFACT_DRIVEN",
        ]
    );
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
fn flow_lock_id_is_deterministic_from_normalized_content_and_check_mode() {
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
    assert!(yaml.starts_with("id: "));
    assert!(yaml.contains(lock.id()));
}

#[test]
fn run_compatibility_reports_unavailable_resources() {
    let input = RunCompatibility {
        available_resources: vec!["readme.main".into(), "schema.handoff".into()],
    };

    let result = flow_check_run_compatibility(&valid_draft(), input);

    assert!(!result.compatible);
    assert_eq!(
        diagnostic_codes(&result.diagnostics),
        vec!["FLOW_RUN_RESOURCE_UNAVAILABLE"]
    );
}
