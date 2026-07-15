use humanize_plugin::{
    adapters::tmux::{PaneActivation, TmuxAdapter},
    flow::{FlowDraft, flow_check},
    kernel::{Artifact, Board, Contract, Event, Node, kernel_primitive_names},
    mcp::McpSurface,
    runtime::{Activation, EventStore, LocalEventStore},
};

#[test]
fn kernel_value_types_are_importable() {
    let _node = Node::default();
    let _contract = Contract::default();
    let _artifact = Artifact::default();
    let _board = Board::default();
    let _event = Event::default();
    assert!(kernel_primitive_names().contains(&"Route"));
}

#[test]
fn outer_architecture_boundaries_are_importable() {
    let draft = FlowDraft::default();
    let _report = flow_check(&draft, Default::default());
    let _activation = Activation::default();
    let event_store = LocalEventStore::default();
    let _mcp = McpSurface;
    let _tmux = TmuxAdapter::default();
    let _pane_activation = PaneActivation::default();

    assert_event_store(&event_store);
}

fn assert_event_store<T: EventStore>(_store: &T) {}
