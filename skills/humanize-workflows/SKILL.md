---
name: humanize-workflows
description: Use when a coding agent is asked to use Humanize, workflow tooling, flow packages, MCP workflow tools, or terse natural-language flow authoring in a repository.
---

# Humanize Workflows

## Overview

Humanize turns a terse natural-language request into a checked, locked,
reviewed, and runnable flow package. When Humanize is requested, use the
Humanize MCP tools instead of replacing the workflow with ordinary repository
exploration.

## Required Order

1. Call `flow_suggest` with the user's terse goal.
2. Validate the draft with `flow_check`.
3. Confirm the flow package includes a README resource.
4. Lock the validated draft with `flow_lock`.
5. Export the locked flow with `flow_export` when a run artifact directory is available.
6. Prepare the review gate with `prepare_flow_review`.
7. Ask for human approval unless the user explicitly permits bypass.
8. Record the decision with `decide_flow_review` using `approved` or `bypassed`.
9. Run with `run_flow` only after the review gate is recorded.

## MCP Interaction Pattern

Use MCP tools as the compiler and runtime interface:

1. `flow_suggest`: get a valid skeleton from the terse task.
2. Edit the returned draft in memory to add nodes, contracts, actions,
   work profiles, resources, QoS intent, and routes that fit the task.
3. `flow_check`: validate the full draft. Use `strict` when the flow will be
   shared or run for a long time.
4. `flow_repair`: ask for unranked local candidates plus guidance and
   diagnostics. Choose among candidates yourself; do not infer a ranking.
5. `flow_lock`: freeze the checked draft.
6. `flow_export`: save the locked flow when artifacts should be collected.
7. `prepare_flow_review`: create the human review view before execution.
8. `decide_flow_review`: record either explicit approval or an explicit
   bypass.
9. `run_flow`: start the runtime driver.
10. During a run, use `preview_flow_routes`, `propose_flow_update`, and
   `apply_flow_update` when observations justify changing the flow.

For long-running tasks, prefer entering `run_flow` quickly with a small,
validated adaptive loop over spending many turns hand-authoring a large
perfect-looking graph.

## Executable tmux Runs

The runtime autonomously maintains agent-backed nodes through tmux. Configure
the MCP server with an execution context before starting a long-running flow:

```sh
export HUMANIZE_TMUX_SESSION="$(tmux display-message -p '#S')"
export HUMANIZE_AGENT_COMMAND="agent-command"
export HUMANIZE_TMUX_BIN="/usr/bin/tmux"
```

`HUMANIZE_TMUX_WINDOW` is optional; when it is absent, `run_flow` uses the
`run_id` as the window name. A call with only `run_id`, `flow_lock_id`, and
review identifiers is valid when these defaults are configured. If the context
is missing, `run_flow` fails before starting the run.

An explicit `tmux` object overrides these defaults and can gate prompt
submission on an interactive agent's readiness marker:

```json
{
  "run_id": "task-run",
  "flow_lock_id": "flow-lock-id",
  "tmux": {
    "enabled": true,
    "session": "current-session-name",
    "window": "task-run",
    "agent_command": "codex --dangerously-bypass-approvals-and-sandbox",
    "agent_ready_pattern": "gpt-5.6-sol ultra",
    "agent_ready_timeout_ms": 60000,
    "prompt_submit_key_count": 2
  }
}
```

Use `tmux display-message -p '#S'` to discover the current session. The coding
agent inherits the container environment and its installed Humanize MCP
configuration. Configure `agent_ready_pattern` for interactive agents so the
node prompt is submitted only after both the native `SessionStart` binding and
the TUI marker are ready. The native binding remains the lifecycle authority;
pane text never creates session facts. Set `prompt_submit_key_count` to the
number of Enter keys that agent requires.

Install the production client hooks before autonomous runs:

```sh
humanize-plugin-mcp --print-client-config codex-hooks-json --command "$(command -v humanize-plugin-mcp)"
humanize-plugin-mcp --print-client-config claude-hooks-json --command "$(command -v humanize-plugin-mcp)"
```

Use the generated `SessionStart` hook to bind the native coding session to the
exact driver-owned allocation, the generated `Stop` hook to obtain the durable
completion decision, and the generated `PreToolUse` hook to deny direct
`tmux send-keys` into owned panes. If shell hooks can be bypassed, put a wrapper
named `tmux` earlier on
the agent `PATH` that runs:

```sh
exec humanize-plugin-mcp --guarded-tmux -- "$@"
```

Keep `HUMANIZE_TMUX_BIN` pointed at the real tmux binary so Humanize MCP
transport bypasses the wrapper. Do not pass run or activation identifiers to
node prompts; ownership is discovered from durable pane state.

The driver pane is an operator-only interactive console. Do not put its pane
identity or mutable console commands in participant or master prompts;
model control goes through MCP. Hooks and wrappers enforce cooperative policy for
the supported harness and literal shell command boundary, not a
same-UID OS security boundary.

Existing-run tools attach to the authenticated driver automatically. If it has
exited, MCP serializes replacement startup and replays durable state. Use
`resume_run` for public recovery and reconciliation; do not invent a manual
driver restart operation.

Choose `run_mode` deliberately. `finite` completes when no activation or fact
trigger remains. `continuous` becomes quiescent and wakes on a new fact
version. `manual` remains quiescent until `resume_run`; call `complete_run`
only when that run is quiescent. Treat `activation_limit` as an absolute total,
not a per-resume allowance, and only raise it when resuming a budget pause.

Activation ids are opaque across repeated fact generations. Pane replacement
does not create a new activation; it creates a new allocation generation, so
old readiness, capture, and input receipts must not be reused for the new pane.
Agent and review action drivers are agent-backed and are actuated through tmux.
Script action drivers are rejected before lock until they have an explicit
runtime execution contract. For deterministic shell work, use an agent-backed
node that runs the command, records the artifact, and stops under its contract.
Treat any actuation warning as an execution gap, not as successful node
completion.

### Ambiguous delivery recovery

When status contains `ambiguous_delivery`, read its
`started_event_sequence`. Resolve only that barrier through `resume_run` with
`delivery_resolution` containing the sequence, `outcome` set to `submitted`
or `not_submitted`, and non-empty `evidence`. Use `submitted` only with
receiver-side confirmation; it closes the barrier without replay. Use
`not_submitted` only with evidence that retry is safe. Never resend directly,
and refresh status after a stale-sequence conflict.

## Flow Architecture

Design the smallest flow that gives the task durable control. Use the Humanize
primitives directly:

| Primitive | Purpose |
| --- | --- |
| `nodes` | Work units: agent, script, review, or human. Add `work_profile` when intent or execution traits matter. |
| `contracts` | Required delivery for a node. Use `all_artifacts` when stopping must depend on artifacts. |
| `routes` | Runtime activation rules. Use typed `exists` or `truthy` predicates over artifact or board facts. |
| `resources` | README, prompts, schemas, scripts, views, and packaged subflows. |
| `imports` | Reusable flow resources or schema aliases. |
| `policies` | Write boundaries for artifacts, resources, workspace, and system state. |
| `qos` | Run intent: `interactive`, `standard`, or `background`, with optional `completion_target`. |

Prefer a small graph over a large instruction blob. A good flow has a frozen
task contract, bounded work nodes, explicit artifacts, measurable checks, and
a review or guard that decides whether to continue, branch, or finish.

## Design Heuristics

- Start by naming the durable value: bug fix, benchmark improvement, migration
  patch, audit report, release packet, reproduction evidence, or test suite.
- Freeze the operator-owned task contract early with an agent-backed node.
  Later nodes should read this artifact instead of reinterpreting the original
  request.
- Make each agent node own one coherent lane. Use separate lanes for
  investigation, implementation, testing, documentation, performance
  hypotheses, or candidate variants.
- Use agent-backed nodes for deterministic checks, materialization, benchmark
  runs, route classification, archive creation, and evidence guards.
- Use review nodes for judgment: accept, revise, reject, promote, continue, or
  finish. Review nodes are agent-backed and should write a verdict artifact
  that routes can test.
- Use routes for adaptation. Branch on reproduced vs not reproduced, patch vs
  no-code evidence, benchmark winner, review verdict, missing artifact, or
  validation failure.
- Use `for_each` only when a collection is real: hypotheses, failing tests,
  changed modules, benchmark candidates, or independent work lanes.
- Keep loops bounded by contract and evidence. A loop should have a concrete
  repair target, a validation signal, and an archive path for terminal state.
- Put long instructions in prompt resources, not in the user-facing request.
  Keep the top-level flow draft readable.
- Include exactly one root `README.md` resource in every package. The main
  agent must author and supply it; preserve that content verbatim and never
  generate or repair it from the goal.
- Use Work Profile intents `produce`, `evaluate`, `explore`, `synthesize`, and
  `coordinate` to describe node work. Set workspace, tool, and network access
  traits only when the default would be misleading.
- Use `record_hook_fact` for native hook telemetry such as
  `compaction_pending` and `compaction_finished`; it updates the session
  `context_generation` without changing runtime state.

## Common Shapes

| Task shape | Flow shape |
| --- | --- |
| Bug repair | Freeze task -> triage -> reproduce -> isolate cause -> patch or no-code evidence -> regression -> review -> repair loop or archive. |
| Performance work | Freeze task -> capture baseline -> generate hypotheses -> run candidates in parallel -> benchmark -> select or repair -> review loop -> archive. |
| Parallel implementation | Freeze task -> scope plan -> parallel lanes -> lane guard -> integration review -> validation -> final review. |
| Research reproduction | Freeze claim -> setup -> baseline run -> variant or negative control -> comparison -> review loop -> archive evidence. |
| Documentation or tests | Inventory -> gap report -> generate or patch -> validation -> review -> repair loop or archive. |

## Minimal Draft Example

For a measured optimization task, use a shape like this:

```json
{
  "nodes": [
    {
      "id": "capture_baseline",
      "contract_id": "contract.baseline",
      "action": {
        "driver": "agent",
        "prompt_ref": "prompts/baseline.md",
        "writes": ["artifact.baseline"]
      }
    },
    {
      "id": "try_candidates",
      "contract_id": "contract.candidates",
      "action": {
        "driver": "agent",
        "prompt_ref": "prompts/optimize.md",
        "reads": ["artifact.baseline"],
        "writes": ["artifact.candidates"]
      }
    },
    {
      "id": "review_selection",
      "contract_id": "contract.review",
      "action": {
        "driver": "review",
        "prompt_ref": "prompts/review.md",
        "reads": ["artifact.baseline", "artifact.candidates"],
        "writes": ["artifact.review_verdict", "artifact.review_continue"],
        "verdict_artifact": "artifact.review_verdict"
      }
    }
  ],
  "contracts": [
    {
      "id": "contract.baseline",
      "completion": "all_artifacts",
      "artifacts": [
        {
          "id": "baseline",
          "schema_resource_id": "schemas/baseline.txt"
        }
      ]
    },
    {
      "id": "contract.candidates",
      "completion": "all_artifacts",
      "artifacts": [
        {
          "id": "candidates",
          "schema_resource_id": "schemas/candidates.txt"
        }
      ]
    },
    {
      "id": "contract.review",
      "completion": "all_artifacts",
      "artifacts": [
        {
          "id": "review_verdict",
          "schema_resource_id": "schemas/review-verdict.txt"
        }
      ]
    }
  ],
  "routes": [
    {
      "predicate": {
        "op": "exists",
        "fact": {"kind": "artifact", "key": "baseline"}
      },
      "activate": "try_candidates"
    },
    {
      "predicate": {
        "op": "exists",
        "fact": {"kind": "artifact", "key": "candidates"}
      },
      "activate": "review_selection"
    },
    {
      "predicate": {
        "op": "exists",
        "fact": {"kind": "artifact", "key": "review_continue"}
      },
      "activate": "try_candidates"
    }
  ],
  "resources": [
    {
      "path": "README.md",
      "kind": "readme",
      "content": "Measured optimization flow with baseline, candidate search, review, and loop."
    },
    {
      "path": "prompts/baseline.md",
      "kind": "prompt",
      "content": "Run the benchmark command and deliver the result with artifact_key \"baseline\"."
    },
    {
      "path": "prompts/optimize.md",
      "kind": "prompt",
      "content": "Generate and test one bounded candidate improvement, then deliver it with artifact_key \"candidates\"."
    },
    {
      "path": "prompts/review.md",
      "kind": "prompt",
      "content": "Deliver artifact_key \"review_verdict\" with finish or continue. Also deliver artifact_key \"review_continue\" only when another candidate is needed."
    },
    {
      "path": "schemas/baseline.txt",
      "kind": "schema",
      "content": "baseline metrics and command evidence"
    },
    {
      "path": "schemas/candidates.txt",
      "kind": "schema",
      "content": "candidate changes with benchmark evidence"
    },
    {
      "path": "schemas/review-verdict.txt",
      "kind": "schema",
      "content": "verdict string finish or continue with reason"
    },
    {
      "path": "schemas/review-continue.txt",
      "kind": "schema",
      "content": "optional continue marker with reason"
    }
  ]
}
```

This is only a shape. Adapt the node names, artifacts, resources, and routes to
the task. Keep contracts and schemas aligned with the actual artifacts. In this
example, the review node finishes by delivering only artifact key
`review_verdict`; it continues by also delivering artifact key
`review_continue`, which triggers another candidate run.

## Contract Rules

- Every node that must deliver something should have a contract. If the main
  output is side-effect work, use `manual` completion and still record evidence.
- The main agent must author the package's root `README.md`. Do not synthesize,
  repair, or replace it from the terse user goal.
- Flow fact paths should be semantic and stable, such as `artifact.baseline`,
  `artifact.hypotheses`, `artifact.validation`, `artifact.review_verdict`, and
  `artifact.archive`. When calling `deliver_artifact`, use the bare artifact id,
  such as `baseline`, not the fact path `artifact.baseline`.
- Schemas can be lightweight, but every required artifact should have either a
  schema resource or a concise prompt resource describing the expected shape.
- Review and guard outputs should be route-friendly: use small verdict strings
  or structured fields instead of prose-only summaries.
- Do not use Humanize as decoration after ordinary implementation has already
  started. The flow should be the execution plan.

## Quick Reference

| Situation | Use |
| --- | --- |
| User says "use Humanize" or "use workflow" | Start with `flow_suggest`. |
| Draft flow exists | Run `flow_check` before locking. |
| Flow package lacks README | Repair before `flow_lock`. |
| Long-running or effectful execution | Use `prepare_flow_review` first. |
| User approved or allowed bypass | Record with `decide_flow_review`, then `run_flow`. |

## Common Mistakes

- Do not start with ordinary file search and call Humanize later as decoration.
- Do not run a flow before `flow_check`, `flow_lock`, and the review gate.
- Do not lock a flow package that lacks a README.
- Do not treat a bypass as implicit; record it before execution.
