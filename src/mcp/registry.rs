use serde_json::Value;

use crate::driver::protocol::DriverWire;

use super::driver_proxy::{
    ArgumentPreparer, prepare_apply_flow_lock_arguments, prepare_apply_flow_update_arguments,
    prepare_artifact_arguments, prepare_authoring_arguments, prepare_board_patch_arguments,
    prepare_driver_arguments, prepare_effect_arguments, prepare_fanout_arguments,
    prepare_hook_fact_arguments, prepare_node_arguments, prepare_participant_message_arguments,
    prepare_preview_flow_routes_arguments,
};
use super::tool_schemas;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum ToolCategory {
    Runtime,
    Authoring,
    Review,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum CallerKind {
    Operator,
    Participant,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum CallerScope {
    OperatorOnly,
    OperatorAndParticipant,
    HookOnly,
    Hidden,
}

impl CallerScope {
    pub(super) const fn allows(self, caller: CallerKind) -> bool {
        match self {
            Self::OperatorOnly => matches!(caller, CallerKind::Operator),
            Self::OperatorAndParticipant => true,
            Self::HookOnly | Self::Hidden => false,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum AuthoringOperation {
    FlowRepair,
    FlowApply,
    FlowSuggest,
    FlowCheck,
    FlowLock,
    FlowExport,
    ProposeFlowUpdate,
    PrepareFlowReview,
    DecideFlowReview,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum ToolRoute {
    Authoring(AuthoringOperation),
    DriverRead(DriverWire),
    DriverMutation { wire: DriverWire, bootstrap: bool },
    ParticipantMessage(DriverWire),
    Hidden,
}

impl ToolRoute {
    pub(super) const fn wire(self) -> Option<DriverWire> {
        match self {
            Self::DriverRead(wire)
            | Self::DriverMutation { wire, .. }
            | Self::ParticipantMessage(wire) => Some(wire),
            Self::Authoring(_) | Self::Hidden => None,
        }
    }
}

pub(super) type SchemaBuilder = fn() -> Value;

#[derive(Debug, Clone, Copy)]
pub(super) struct ToolSpec {
    pub(super) name: &'static str,
    pub(super) category: ToolCategory,
    pub(super) route: ToolRoute,
    pub(super) caller_scope: CallerScope,
    pub(super) description: &'static str,
    pub(super) input_schema: SchemaBuilder,
    pub(super) participant_input_schema: Option<SchemaBuilder>,
    pub(super) prepare_arguments: ArgumentPreparer,
}

impl ToolSpec {
    pub(super) const fn is_advertised_for(&self, caller: CallerKind) -> bool {
        self.caller_scope.allows(caller)
    }

    pub(super) fn input_schema_for(&self, caller: CallerKind) -> Value {
        match caller {
            CallerKind::Participant => self
                .participant_input_schema
                .map(|builder| builder())
                .unwrap_or_else(|| (self.input_schema)()),
            CallerKind::Operator => (self.input_schema)(),
        }
    }
}

const fn local(
    name: &'static str,
    category: ToolCategory,
    operation: AuthoringOperation,
    description: &'static str,
    input_schema: SchemaBuilder,
) -> ToolSpec {
    ToolSpec {
        name,
        category,
        route: ToolRoute::Authoring(operation),
        caller_scope: CallerScope::OperatorOnly,
        description,
        input_schema,
        participant_input_schema: None,
        prepare_arguments: prepare_authoring_arguments,
    }
}

const fn read(
    name: &'static str,
    wire: DriverWire,
    description: &'static str,
    input_schema: SchemaBuilder,
    prepare_arguments: ArgumentPreparer,
) -> ToolSpec {
    ToolSpec {
        name,
        category: ToolCategory::Runtime,
        route: ToolRoute::DriverRead(wire),
        caller_scope: CallerScope::OperatorOnly,
        description,
        input_schema,
        participant_input_schema: None,
        prepare_arguments,
    }
}

const fn participant_read(
    name: &'static str,
    wire: DriverWire,
    description: &'static str,
    input_schema: SchemaBuilder,
    prepare_arguments: ArgumentPreparer,
    participant_input_schema: SchemaBuilder,
) -> ToolSpec {
    let mut spec = read(name, wire, description, input_schema, prepare_arguments);
    spec.caller_scope = CallerScope::OperatorAndParticipant;
    spec.participant_input_schema = Some(participant_input_schema);
    spec
}

const fn mutation(
    name: &'static str,
    wire: DriverWire,
    description: &'static str,
    input_schema: SchemaBuilder,
    prepare_arguments: ArgumentPreparer,
) -> ToolSpec {
    ToolSpec {
        name,
        category: ToolCategory::Runtime,
        route: ToolRoute::DriverMutation {
            wire,
            bootstrap: false,
        },
        caller_scope: CallerScope::OperatorOnly,
        description,
        input_schema,
        participant_input_schema: None,
        prepare_arguments,
    }
}

const fn participant_mutation(
    name: &'static str,
    wire: DriverWire,
    description: &'static str,
    input_schema: SchemaBuilder,
    prepare_arguments: ArgumentPreparer,
    participant_input_schema: SchemaBuilder,
) -> ToolSpec {
    let mut spec = mutation(name, wire, description, input_schema, prepare_arguments);
    spec.caller_scope = CallerScope::OperatorAndParticipant;
    spec.participant_input_schema = Some(participant_input_schema);
    spec
}

const fn hook_mutation(
    name: &'static str,
    wire: DriverWire,
    description: &'static str,
    input_schema: SchemaBuilder,
    prepare_arguments: ArgumentPreparer,
) -> ToolSpec {
    let mut spec = mutation(name, wire, description, input_schema, prepare_arguments);
    spec.caller_scope = CallerScope::HookOnly;
    spec
}

const fn bootstrap(
    name: &'static str,
    wire: DriverWire,
    description: &'static str,
    input_schema: SchemaBuilder,
) -> ToolSpec {
    ToolSpec {
        name,
        category: ToolCategory::Runtime,
        route: ToolRoute::DriverMutation {
            wire,
            bootstrap: true,
        },
        caller_scope: CallerScope::OperatorOnly,
        description,
        input_schema,
        participant_input_schema: None,
        prepare_arguments: prepare_driver_arguments,
    }
}

const fn message(
    name: &'static str,
    wire: DriverWire,
    description: &'static str,
    input_schema: SchemaBuilder,
) -> ToolSpec {
    ToolSpec {
        name,
        category: ToolCategory::Runtime,
        route: ToolRoute::ParticipantMessage(wire),
        caller_scope: CallerScope::OperatorOnly,
        description,
        input_schema,
        participant_input_schema: None,
        prepare_arguments: prepare_participant_message_arguments,
    }
}

const fn hidden(
    name: &'static str,
    description: &'static str,
    input_schema: SchemaBuilder,
) -> ToolSpec {
    ToolSpec {
        name,
        category: ToolCategory::Runtime,
        route: ToolRoute::Hidden,
        caller_scope: CallerScope::Hidden,
        description,
        input_schema,
        participant_input_schema: None,
        prepare_arguments: prepare_authoring_arguments,
    }
}

pub(super) const TOOL_SPECS: &[ToolSpec] = &[
    hidden(
        "start_run",
        "Call start_run to create a runtime run before using runtime tools; omit nodes to create root.",
        tool_schemas::start_run,
    ),
    participant_read(
        "get_context",
        DriverWire::Context,
        "Return authoritative context for one driver-owned run.",
        tool_schemas::run_id,
        prepare_driver_arguments,
        tool_schemas::participant_context,
    ),
    participant_mutation(
        "deliver_artifact",
        DriverWire::DeliverArtifact,
        "Record an artifact payload for a run activation.",
        tool_schemas::deliver_artifact,
        prepare_artifact_arguments,
        tool_schemas::participant_deliver_artifact,
    ),
    mutation(
        "fanout_from_artifact",
        DriverWire::Fanout,
        "Create one runtime activation per line in the latest artifact slot.",
        tool_schemas::fanout_from_artifact,
        prepare_fanout_arguments,
    ),
    participant_mutation(
        "record_effect",
        DriverWire::RecordEffect,
        "Record an effect fact for a run activation.",
        tool_schemas::record_effect,
        prepare_effect_arguments,
        tool_schemas::participant_record_effect,
    ),
    hook_mutation(
        "record_hook_fact",
        DriverWire::RecordHookFact,
        "Record a native hook fact for a run session without changing runtime state.",
        tool_schemas::record_hook_fact,
        prepare_hook_fact_arguments,
    ),
    mutation(
        "patch_board",
        DriverWire::PatchBoard,
        "Patch local run board values with optional version checking.",
        tool_schemas::patch_board,
        prepare_board_patch_arguments,
    ),
    mutation(
        "activate_node",
        DriverWire::Activate,
        "Create runtime metadata for a node activation.",
        tool_schemas::activate_node,
        prepare_node_arguments,
    ),
    message(
        "send_message",
        DriverWire::SendMessage,
        "Deliver one idempotent targeted message to a running driver-owned activation.",
        tool_schemas::send_message,
    ),
    participant_read(
        "validate_stop",
        DriverWire::ValidateStop,
        "Validate whether a runtime activation satisfies its local stop contract.",
        tool_schemas::validate_stop,
        prepare_driver_arguments,
        tool_schemas::participant_validate_stop,
    ),
    hook_mutation(
        "observe_stop",
        DriverWire::ObserveStop,
        "Record an observed activation stop and let the runtime driver validate completion.",
        tool_schemas::observe_stop,
        prepare_driver_arguments,
    ),
    mutation(
        "apply_flow_lock",
        DriverWire::ApplyFlowRevision,
        "Apply an exact flow lock package to an existing driver-owned run; create runs with run_flow.",
        tool_schemas::apply_flow_lock,
        prepare_apply_flow_lock_arguments,
    ),
    read(
        "preview_flow_routes",
        DriverWire::PreviewFlowRoutes,
        "Preview an exact flow lock package against a driver-owned run without mutation; create runs with run_flow.",
        tool_schemas::preview_flow_routes,
        prepare_preview_flow_routes_arguments,
    ),
    bootstrap(
        "run_flow",
        DriverWire::BindRun,
        "Create a runtime run through the driver control surface, with optional flow review enforcement.",
        tool_schemas::run_flow,
    ),
    read(
        "run_status",
        DriverWire::Status,
        "Return driver run status and runtime context for one run.",
        tool_schemas::run_id,
        prepare_driver_arguments,
    ),
    read(
        "run_why",
        DriverWire::Why,
        "Return a concise reason for the current run state.",
        tool_schemas::run_id,
        prepare_driver_arguments,
    ),
    mutation(
        "pause_run",
        DriverWire::Pause,
        "Pause an existing run through the runtime driver control path.",
        tool_schemas::run_id,
        prepare_driver_arguments,
    ),
    mutation(
        "resume_run",
        DriverWire::Resume,
        "Resume an existing run. If status reports ambiguous_delivery, pass delivery_resolution with that barrier's started_event_sequence, outcome submitted or not_submitted, and non-empty evidence. A stale sequence returns delivery_barrier_conflict without mutation; unresolved barriers remain in status context.",
        tool_schemas::resume_run,
        prepare_driver_arguments,
    ),
    mutation(
        "complete_run",
        DriverWire::Complete,
        "Complete a quiescent run through the runtime driver control path.",
        tool_schemas::run_id,
        prepare_driver_arguments,
    ),
    mutation(
        "stop_run",
        DriverWire::Stop,
        "Request run stopping through the runtime driver control path.",
        tool_schemas::run_id,
        prepare_driver_arguments,
    ),
    read(
        "view_terminal",
        DriverWire::ViewTerminal,
        "Render one authoritative driver-owned runtime snapshot as terminal text.",
        tool_schemas::run_id,
        prepare_driver_arguments,
    ),
    read(
        "view_snapshot",
        DriverWire::ViewSnapshot,
        "Return one authoritative driver-owned runtime snapshot as structured JSON.",
        tool_schemas::run_id,
        prepare_driver_arguments,
    ),
    hidden(
        "view_browser",
        "Serve the current in-memory runtime snapshot on a local read-only HTTP port.",
        tool_schemas::view_browser,
    ),
    local(
        "flow_repair",
        ToolCategory::Authoring,
        AuthoringOperation::FlowRepair,
        "Run flow authoring repair analysis; does not modify state and returns unranked safe local candidates in authored order plus guidance and diagnostics.",
        tool_schemas::flow_repair,
    ),
    local(
        "flow_apply",
        ToolCategory::Authoring,
        AuthoringOperation::FlowApply,
        "Record that a supplied or locked flow was selected for application.",
        tool_schemas::flow_apply,
    ),
    local(
        "flow_suggest",
        ToolCategory::Authoring,
        AuthoringOperation::FlowSuggest,
        "Humanize entry for terse natural-language workflow requests. Use first when the user asks to design or use a Humanize flow; then call flow_check, flow_lock, prepare_flow_review, and run_flow as needed.",
        tool_schemas::flow_suggest,
    ),
    local(
        "flow_check",
        ToolCategory::Authoring,
        AuthoringOperation::FlowCheck,
        "Validate a Humanize flow draft before locking, review, export, or runtime execution.",
        tool_schemas::flow_draft,
    ),
    local(
        "flow_lock",
        ToolCategory::Authoring,
        AuthoringOperation::FlowLock,
        "Freeze a validated Humanize flow draft into a deterministic lock for review, export, and runtime execution.",
        tool_schemas::flow_draft,
    ),
    local(
        "flow_export",
        ToolCategory::Authoring,
        AuthoringOperation::FlowExport,
        "Export a known flow lock through the flow authoring exporter.",
        tool_schemas::flow_export,
    ),
    local(
        "propose_flow_update",
        ToolCategory::Authoring,
        AuthoringOperation::ProposeFlowUpdate,
        "Check and lock a candidate flow update before runtime application.",
        tool_schemas::propose_flow_update,
    ),
    mutation(
        "apply_flow_update",
        DriverWire::ApplyFlowRevision,
        "Apply a previously proposed flow update to an existing runtime run.",
        tool_schemas::apply_flow_update,
        prepare_apply_flow_update_arguments,
    ),
    local(
        "prepare_flow_review",
        ToolCategory::Review,
        AuthoringOperation::PrepareFlowReview,
        "Create a human-readable review document for a Humanize flow lock or draft before long-running execution.",
        tool_schemas::prepare_flow_review,
    ),
    local(
        "decide_flow_review",
        ToolCategory::Review,
        AuthoringOperation::DecideFlowReview,
        "Record the human review decision for a Humanize flow before reviewed runtime execution.",
        tool_schemas::decide_flow_review,
    ),
];

pub(super) fn spec(name: &str) -> Option<&'static ToolSpec> {
    TOOL_SPECS.iter().find(|spec| spec.name == name)
}

pub(super) fn advertised_spec(name: &str) -> Option<&'static ToolSpec> {
    advertised_spec_for(name, CallerKind::Operator)
}

pub(super) fn advertised_specs() -> impl Iterator<Item = &'static ToolSpec> {
    advertised_specs_for(CallerKind::Operator)
}

pub(super) fn advertised_spec_for(name: &str, caller: CallerKind) -> Option<&'static ToolSpec> {
    spec(name).filter(|spec| spec.is_advertised_for(caller))
}

pub(super) fn advertised_specs_for(caller: CallerKind) -> impl Iterator<Item = &'static ToolSpec> {
    TOOL_SPECS
        .iter()
        .filter(move |spec| spec.is_advertised_for(caller))
}

pub(super) fn advertised_specs_in(
    category: ToolCategory,
) -> impl Iterator<Item = &'static ToolSpec> {
    advertised_specs().filter(move |spec| spec.category == category)
}
