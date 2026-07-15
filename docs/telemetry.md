# Public Telemetry

Humanize run telemetry has one public time-series authority:
`records/events.jsonl` under the public run root. It is an append-only JSONL
journal with schema name `humanize.public_journal.event` and schema major `1`.
Each event has a contiguous global `seq`, stable `event_id`,
`occurred_at_ms`, salted pseudonymous `run_ref`, optional `activation_ref`,
`session_ref`, and `revision_ref`, a typed `kind`, optional causal fields, and
typed `data`. Raw run, activation, native session, tmux, and revision
identifiers are never public event fields.

Wall-clock time is display metadata. Replay, ordering, and causal checks use
`seq`.

## Public Journal

The journal covers durable facts from the runtime, flow revisions, activations,
native participant sessions, hooks, compaction observations, machine input
delivery, stop observations, stop decisions, and route decisions. Session
events describe only factual native-session start, binding, and exit. Human
intervention, work profiles, QoS, and usage are not synthesized from tmux,
readiness, or runtime state.

Split record files such as flow, topology, QoS, hook, machine-input, or tmux
JSONL streams are not public authorities. Public projections and manifest
summaries are derived from `records/events.jsonl`.

Public producers construct closed typed event variants. The serialized `kind`
strings and `data` object shapes are stable for schema major `1`; readers
reject malformed known payloads and unknown schema majors, while preserving
unknown same-major event kinds and additive fields as opaque evidence for
forward compatibility.

The per-run driver is the only production public-journal writer. MCP, native
hooks, and tmux guards submit authenticated requests through driver IPC or a
private durable inbox. They never append public records directly.

Writers may keep a private validated cursor in memory with byte length, last
sequence, current hash state, and idempotency identities. That cursor is only
an optimization. If it is missing or no longer matches the public journal on
disk, it is rebuilt from one scan of `records/events.jsonl`; the journal
remains the authority.

## Content Authority

The journal references content; it does not duplicate content authority.

- Flow revision files own canonical flow content.
- Immutable content-addressed files own published artifact and fact content.
- Journal events carry canonical hashes, relative refs, lengths, and typed
  allowlisted fields.

Prompt, hook payload, machine input, and artifact fields default to hash and
length unless a typed field is explicitly allowlisted. Raw commands, binding
paths, executable paths, native identifiers, nonces, tokens, and credentials
are not public. Events without a real source are not written.

## Manifest Summary

The public manifest declares the journal schema, relative path, status,
event count, last sequence, current SHA-256, and final SHA-256 when sealed.

Continuous runs remain `open` and may keep appending. Terminal runs seal the
journal atomically. If a torn tail is found, the committed prefix is recovered,
the tail is quarantined under `records/quarantine`, and the manifest reports a
corrupt sealed journal instead of silently hiding the condition.

## Public And Private Roots

The public run root contains only public run assets: the journal and seal,
relative-path projections, immutable flow revisions, content-addressed public
content, and quarantine evidence. Activation metadata, transcripts, final
captures, and machine-input ledgers are private.

Driver actuation and recovery state lives under the private runtime root for
the run. That includes driver event logs, snapshots, IPC metadata, IPC tokens,
socket metadata, participant credentials, readiness nonces, native session raw
identifiers, activation captures, private machine-input ledgers, and the
ordered publication outbox and ledger. The outbox is persisted before private
mutation; pending publication blocks later mutation and is replayed on
restart. Public session identifiers use salted stable pseudonymous hashes; raw
mappings stay private.

The review MAC key and review authority remain in user state outside the run
root.

## Flow-Aware Facts

Flow-aware facts are typed local observations. They record revision and
topology decisions, planned and applied fanout, activation lifecycle, route
and fact observations, native-session lifecycle, and compaction. They do not
create a serving side channel or infer telemetry that was not observed.
