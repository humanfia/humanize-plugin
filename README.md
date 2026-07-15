# humanize-plugin

Humanize turns terse local workflow requests into checked, reviewed, and
runnable MCP flow packages.

## Install

Install the MCP runtime and its dedicated per-run driver from GitHub:

```bash
cargo install --git https://github.com/humanfia/humanize-plugin --locked --bins
```

Add the marketplace and plugin to Codex:

```bash
codex plugin marketplace add humanfia/humanize-plugin
codex plugin add humanize-plugin@humanfia
```

Add the marketplace and plugin to Claude Code:

```bash
claude plugin marketplace add humanfia/humanize-plugin
claude plugin install humanize-plugin@humanfia
```

## First Use

Use terse prompts that name Humanize or workflow:

```text
Use Humanize to audit this C library without editing files.
Use Humanize to split this refactor into a reviewed workflow and run it after I approve.
Use workflow to package this task with a README, check it, lock it, review it, then run.
```

Humanize authoring normally starts with `flow_suggest`, then uses `flow_check`,
`flow_lock`, `prepare_flow_review`, `decide_flow_review`, and `run_flow`.
Supply the package's root `README.md` content explicitly; Humanize preserves it
verbatim and does not generate or repair it from the goal.

For agent-backed runs, set the tmux execution context:

```sh
export HUMANIZE_TMUX_SESSION="$(tmux display-message -p '#S')"
export HUMANIZE_AGENT_COMMAND="agent-command"
export HUMANIZE_TMUX_BIN="/usr/bin/tmux"
```

Generate the hook config for the client you use:

```bash
humanize-plugin-mcp --print-client-config codex-hooks-json --command "$(command -v humanize-plugin-mcp)"
humanize-plugin-mcp --print-client-config claude-hooks-json --command "$(command -v humanize-plugin-mcp)"
```

Install the generated block in the client's hook config, then invoke Humanize
with one of the prompts above. Rust 1.88 or newer is required.

See `docs/architecture.md` for package, review, runtime, and repository design.
