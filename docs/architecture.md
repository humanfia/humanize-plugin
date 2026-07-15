# Architecture

humanize-plugin is a local workflow runtime plugin written in Rust. MCP is the
authoring and control surface; one durable driver process is the semantic
authority for each active run. The GitHub repository is both the development
repository and the distribution repository for plugin metadata.

## Kernel Model

The kernel owns six primitives: Node, Contract, Artifact, Board, Route, and
Event. Flow authoring, runtime activation, adapters, profiles, views, and MCP
transport are outer layers that depend on the kernel instead of becoming part
of it.

## Boundaries

- Runs on one local machine.
- Uses Rust as the primary implementation language.
- Exposes an MCP server entrypoint that compiles authoring requests and proxies
  live-run operations to the owning driver.
- Keeps flow authoring and checking separate from runtime execution.
- Uses append-only runtime events and immutable flow revisions as replay
  authority; snapshots are reconstructible caches.
- Maps tmux session to host coding session, window to workflow run, and pane to
  node activation.
- Does not include distributed execution, remote persistence, or cloud service
  integration.

## Flow Packages

`FlowDraft` is the only mutable authoring input. Checking and locking produce a
`FlowLock`, which is an immutable, self-contained package; there is no second
package type. A lock serializes as one direct document containing the
canonical flow, diagnostics, lock id, and content hash. It never embeds a
second JSON document as a string.

The package identity is one SHA-256 digest. The lock id is `flk_<digest>` and
the content hash is `sha256:<digest>`, both derived from the same canonical
bytes. Nodes, routes, resources, imports, scopes, and other semantically
unordered collections are canonicalized before both storage and hashing.
Runtime therefore executes the exact canonical draft covered by the digest;
reordered authoring input cannot produce the same id with different stored
behavior. Deserialization recomputes and validates that identity, so modified
documents are rejected.

A directory package contains `flow.json` plus the flow's root `README.md` and
other declared safe relative resources, including optional
`skills/<name>/SKILL.md` files. The main agent must supply `README.md`; its
content is preserved verbatim and is never generated or repaired from the
goal. Package paths reject absolute paths, parent traversal, symlinks,
hardlinks, duplicate paths, and unresolved resource references. Directories
use mode `0700` and files use mode `0600`. Writes build a complete private
staging directory beside the final package, sync every file and directory,
then publish it with an atomic no-replace rename. Reads walk from held
directory descriptors without following links and reject wrong owners, modes,
types, or link counts.

Package loading always names an explicit directory or supplies the direct lock
document. There is no package registry, archive format, signature layer,
dependency manager, or implicit latest version.

## Predicates And Graphs

Routes use one typed predicate model shared by structured serde parsing,
canonicalization, checking, review, preview, and runtime evaluation. A fact
reference is either an artifact or board value with one validated key. The
only predicates are `exists` and `truthy`; general expressions and a second
predicate language are rejected. `for_each` is a typed artifact reference and
cannot represent a board or arbitrary expression. `exists` tests presence,
while `truthy` also rejects empty text, `false`, and numeric zero.

Visualization is derived from the canonical flow and is never authority. Work
nodes represent flow nodes and fact nodes represent artifact or board facts.
Routes are fact-to-target edges; a distinct fanout artifact contributes its
own fact dependency. When exactly one node produces an artifact, the graph
adds producer-to-fact. External and global facts have no fabricated source
node. Branches, parallel targets, loops, and fanout remain ordinary
compositions of these nodes and edges.

## Review Store

Reviews are durable under the configured user state root. The default is
`$HUMANIZE_STATE_ROOT/reviews`, then
`$XDG_STATE_HOME/humanize/reviews`, then
`$HOME/.humanize/reviews`; tests may inject another root. Explicit
`HUMANIZE_STATE_ROOT`, `XDG_STATE_HOME`, and `HUMANIZE_RUNS_DIR` values are
rejected when they point at the current project directory or a descendant.
The final roots derived from default `HOME` and `XDG_STATE_HOME` are checked as
well, including existing symlink aliases into the project. Private state is
independent of `HUMANIZE_RUNS_DIR`.

`prepared.json` contains the canonical flow package and review binding;
`decision.json` is created once for the terminal decision. Both authority
records are serialized canonically and authenticated with HMAC-SHA-256 using
one random installation key stored as `review-mac.key`. The review root and
review directories use mode `0700`; authority, key, and projection files use
mode `0600`. Descriptor-relative reads reject symlinks, hardlinks, special
files, wrong owners, and wrong modes.

`review.json` and `review.html` are deterministic projections with an explicit
`derived_from` binding. They are atomically regenerated from the canonical
package during prepare and may be replaced when graph rendering changes.
Neither review loading nor driver authorization trusts these projection files.
Browser locations use stable percent-encoded `file://` URIs.

The state transition is `Pending` to `Approved`, `Rejected`, or `Bypassed`.
Terminal states are immutable, and `Rejected` and `Bypassed` require a reason.
`decide_flow_review` is the single decision tool; bypass is never an implicit
boolean option.

The runtime driver accepts a canonical lock and `review_id`, then reads the
review store itself. It rejects missing, pending, rejected, forged, mismatched,
or hash-invalid review bindings, and accepts only approved or explicitly
bypassed bindings. Persisted flow revisions retain the review id and are
reauthorized after driver or MCP restart.

The MAC detects corruption and forgery by processes that do not possess the
installation key. The operating system's same-UID account boundary remains the
trust boundary: this mechanism does not claim to isolate a malicious process
running as the same user, because such a process can read the key.

## Runtime Notes

The durable driver owns runtime progression, tmux pane allocation, agent
actuation, capture finalization, and pane release for one run. It maps the tmux
session to the host coding session, the flow window to the workflow run, and
node panes to activations. MCP processes may reconnect or restart the driver
from durable run metadata and replay state, but do not become runtime
authority. The plugin refuses to use a tmux session named exactly `dev`; use a
dedicated session such as `humanize-plugin-real-test` for local trials.

`RunStarted` records the run mode and absolute activation limit. Activation
usage, route firing, and fact versions are derived from the append-only runtime
event log: artifacts and board keys use their producing event sequence, and a
route trigger is identified by immutable flow lock, canonical route content,
and trigger fact version. Snapshots expose these values but do not own them.

Repeated facts may create later activation generations for the same logical
node lane. Pane replacement is independent: it increments allocation
generation while preserving activation identity. Pane ownership, readiness,
capture, and machine-input records all bind to the exact allocation generation.

Real-test topology is reserved for `humanize-plugin-real-test`: one window per
flow, one pane per project or tool lease, and explicit cleanup for panes,
windows, and the session. The allocator creates that dedicated session fresh
when it has no owned session state. The production MCP path creates only the
flow window and operator pane needed to bootstrap driver authority; the driver
owns subsequent runtime effects.

Agent-backed nodes require `HUMANIZE_TMUX_SESSION`, `HUMANIZE_AGENT_COMMAND`,
and a real `HUMANIZE_TMUX_BIN`. Generated `SessionStart`, `Stop`, and
`PreToolUse` hooks bind native sessions, obtain durable completion decisions,
and deny direct `tmux send-keys` into owned panes. An optional guarded tmux
wrapper strengthens the supported command boundary, but neither hooks nor a
wrapper form a same-UID OS security boundary. The driver pane is operator-only;
participant prompts do not receive its identity or console commands, and model control is routed
through MCP.

Existing-run tools authenticate and attach to the run's driver. If it exits,
MCP serializes replacement startup and replays durable state. Public recovery
uses `resume_run`; there is no separate manual restart operation. When
`run_status` exposes `ambiguous_delivery`, call `resume_run` with a
`delivery_resolution` containing its `started_event_sequence`, an outcome of
`submitted` or `not_submitted`, and non-empty `evidence`. Never resolve it by
blindly resending the command.

Public run telemetry is documented in `docs/telemetry.md`. The public run root
uses `records/events.jsonl` as its only time-series authority; driver actuation,
IPC, credentials, raw native session identifiers, and recovery state live under
the private runtime root for the run. The driver persists each publication
transaction before applying its private mutation, then publishes typed public
facts and acknowledges the transaction. A pending transaction blocks later
mutation and is replayed idempotently after restart. MCP, hooks, and tmux guard
processes submit through driver IPC or private durable inboxes and do not write
the public journal.

## Limitations

- Runtime authority is local and durable under the configured run store.
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
The package installs both the MCP and driver binaries required by production
runtime wiring.
