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
5. Prepare the review gate with `prepare_flow_review`.
6. Ask for human approval unless the user explicitly permits bypass.
7. Record the decision with `approve_flow_review` using `approved` or `bypassed`.
8. Run with `run_flow` only after the review gate is recorded.

## Quick Reference

| Situation | Use |
| --- | --- |
| User says "use Humanize" or "use workflow" | Start with `flow_suggest`. |
| Draft flow exists | Run `flow_check` before locking. |
| Flow package lacks README | Repair before `flow_lock`. |
| Long-running or effectful execution | Use `prepare_flow_review` first. |
| User approved or allowed bypass | Record with `approve_flow_review`, then `run_flow`. |

## Common Mistakes

- Do not start with ordinary file search and call Humanize later as decoration.
- Do not run a flow before `flow_check`, `flow_lock`, and the review gate.
- Do not lock a flow package that lacks a README.
- Do not treat a bypass as implicit; record it before execution.
