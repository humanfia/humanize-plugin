# humanize-plugin

humanize-plugin is a local, MCP-first workflow runtime plugin written in Rust.
It is intended to model workflow state with a small kernel while keeping
execution concerns outside that kernel.

## Thesis

The kernel owns only six primitives: Node, Contract, Artifact, Board, Route,
and Event. Flow authoring, runtime activation, adapters, profiles, views, and
MCP transport are outer layers that depend on the kernel instead of becoming
part of it.

## v0 Boundaries

- Runs on one local machine.
- Uses Rust as the primary implementation language.
- Exposes an MCP server entrypoint as the main local control surface.
- Keeps flow authoring and checking separate from runtime execution.
- Stores runtime events behind an event store boundary.
- Maps tmux session to host coding session, window to workflow run, and pane to
  node activation.
- Does not include distributed execution, remote persistence, or cloud service
  integration.

## Local Build

Run commands from this directory:

```bash
cargo build
cargo test
```

List the MCP tool descriptors exposed by the local binary:

```bash
cargo run --bin humanize-plugin-mcp -- --list-tools
```

After `cargo build`, the binary can also be called directly:

```bash
target/debug/humanize-plugin-mcp --list-tools
```

## Client Config Snippets

The MCP binary can print copyable client setup snippets without changing any
client configuration files:

```bash
cargo run --bin humanize-plugin-mcp -- \
  --print-client-config codex-session \
  --command "$PWD/target/debug/humanize-plugin-mcp"
```

Supported targets are `codex-session`, `codex-persistent`, `claude-project`,
and `claude-session-json`. The helper only prints the requested snippet to
stdout; installation and any persistent config edits remain manual.

## Local MCP Trial

For a session-scoped Codex CLI trial, build the binary first and pass the MCP
server configuration with `-c` overrides:

```bash
cargo build
PLUGIN_MCP="$PWD/target/debug/humanize-plugin-mcp"
codex -C "$PWD" \
  -c "mcp_servers.humanize_plugin.command=\"$PLUGIN_MCP\"" \
  -c 'mcp_servers.humanize_plugin.args=[]'
```

Inside the Codex TUI, use `/mcp` to confirm the `humanize_plugin` server is
loaded for that session. This does not write to `~/.codex/config.toml`.

The local runtime maps tmux session to the host coding session, tmux window to a
workflow run, and tmux pane to a node activation.
The plugin refuses to use a tmux session named exactly `dev`; use a dedicated
session such as `humanize-plugin-real-test` for local trials.
Real-test topology is reserved for `humanize-plugin-real-test`: one window per
flow, one pane per project/tool lease, and explicit cleanup for panes, windows,
and the session. The real-test allocator creates that dedicated session fresh
when it has no owned session state; the ordinary MCP runtime path remains
separate and uses the adapter boundary for host-session and window management.

## Real Trial Prompt

For a real trial, start with a terse natural-language request instead of a
detailed MCP script:

```text
Use Humanize to audit this C library without editing files.
```

A low-capability human-simulator can drive tmux with send/capture operations for
realistic tests while additional panes are created only when a lease is needed.

## v0 Limitations

- Runtime state is local and in-memory.
- MCP authoring tools return minimal local responses suitable for smoke tests.
- Flow locks model local check results and lock provenance, not a distributed
  registry.
- Flow application records lock id, content hash, run id, and application mode;
  it does not migrate active work across machines.
- Tmux integration is an adapter boundary, not a remote scheduler.

## Current Shape

The repository starts as one Rust package in a Cargo workspace. The library
crate defines the kernel, flow, runtime, MCP, and tmux adapter module
boundaries. The binary crate is a minimal MCP entrypoint stub.
