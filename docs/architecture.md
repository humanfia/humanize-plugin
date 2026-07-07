# Architecture

humanize-plugin is a local, MCP-first workflow runtime plugin written in Rust.
The GitHub repository is both the development repository and the distribution
repository for plugin metadata.

## Kernel Model

The kernel owns six primitives: Node, Contract, Artifact, Board, Route, and
Event. Flow authoring, runtime activation, adapters, profiles, views, and MCP
transport are outer layers that depend on the kernel instead of becoming part
of it.

## Boundaries

- Runs on one local machine.
- Uses Rust as the primary implementation language.
- Exposes an MCP server entrypoint as the main local control surface.
- Keeps flow authoring and checking separate from runtime execution.
- Stores runtime events behind an event store boundary.
- Maps tmux session to host coding session, window to workflow run, and pane to
  node activation.
- Does not include distributed execution, remote persistence, or cloud service
  integration.

## Runtime Notes

The local runtime maps tmux session to the host coding session, tmux window to a
workflow run, and tmux pane to a node activation. The plugin refuses to use a
tmux session named exactly `dev`; use a dedicated session such as
`humanize-plugin-real-test` for local trials.

Real-test topology is reserved for `humanize-plugin-real-test`: one window per
flow, one pane per project or tool lease, and explicit cleanup for panes,
windows, and the session. The allocator creates that dedicated session fresh
when it has no owned session state. The ordinary MCP runtime path remains
separate and uses the adapter boundary for host-session and window management.

## Limitations

- Runtime state is local and in-memory.
- MCP authoring tools return minimal local responses suitable for smoke tests.
- Flow locks model local check results and lock provenance, not a distributed
  registry.
- Flow application records lock id, content hash, run id, and application mode;
  it does not migrate active work across machines.
- Tmux integration is an adapter boundary, not a remote scheduler.

## Repository Layout

The repository is one Rust package in a Cargo workspace. The library crate
defines the kernel, flow, runtime, MCP, and tmux adapter module boundaries. The
binary crate is the MCP entrypoint. Distribution metadata lives at the repository
root so the same checkout can be used for development and plugin installation.
