# humanize-plugin

Humanize turns terse local workflow requests into checked, reviewed, and
runnable MCP flow packages.

## Install

Install the MCP runtime binary from GitHub:

```bash
cargo install --git https://github.com/humanfia/humanize-plugin --locked --bin humanize-plugin-mcp
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

## Start with natural language

Use terse prompts that name Humanize or workflow:

```text
Use Humanize to audit this C library without editing files.
Use Humanize to split this refactor into a reviewed workflow and run it after I approve.
Use workflow to package this task with a README, check it, lock it, review it, then run.
```

Humanize authoring normally starts with `flow_suggest`, then uses `flow_check`,
`flow_lock`, `prepare_flow_review`, `approve_flow_review`, and `run_flow`.

## Details

- Requires Rust and Cargo for the runtime install.
- Requires Codex or Claude Code with plugin support enabled.
- Includes `.codex-plugin/plugin.json`, `.agents/plugins/marketplace.json`,
  `.claude-plugin/`, `.mcp.json`, and `skills/` metadata.
- Keeps architecture and repository layout docs in `docs/architecture.md`.

## Update Or Remove

```bash
cargo install --git https://github.com/humanfia/humanize-plugin --locked --bin humanize-plugin-mcp --force
codex plugin marketplace upgrade humanfia
codex plugin add humanize-plugin@humanfia
claude plugin marketplace update humanfia
claude plugin update humanize-plugin
```

```bash
codex plugin remove humanize-plugin@humanfia
codex plugin marketplace remove humanfia
claude plugin uninstall humanize-plugin
claude plugin marketplace remove humanfia
cargo uninstall humanize-plugin
```

## Development Build

```bash
git clone https://github.com/humanfia/humanize-plugin
cd humanize-plugin
cargo build
cargo test
cargo run --bin humanize-plugin-mcp -- --list-tools
```

For local plugin packaging tests from a checkout, use the same commands with
`"$PWD"` instead of `humanfia/humanize-plugin`.

See `docs/architecture.md` for design boundaries and repository layout.
