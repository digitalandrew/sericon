# AI tools and MCP

Local AI coding tools can read retained context, send UART input, and create or run formulas alongside an attached human terminal. Sericon provides a command-line interface and a local Model Context Protocol (MCP) bridge. No AI provider or model key is needed to use Sericon itself.

## Connect a coding tool

Register the local bridge once with the client you use:

```sh
codex mcp add sericon -- "$HOME/.local/bin/sericon" mcp
claude mcp add --transport stdio --scope user sericon -- "$HOME/.local/bin/sericon" mcp
```

Then start `sericon` in your terminal and ask the coding tool to list Sericon
sessions and join the desired one. The bridge attaches to the existing broker;
it never opens a second UART connection. Registration does not start a capture.

## Available operations

The eight session tools are `sericon_sessions`, `sericon_status`, `sericon_read`,
`sericon_send`, `sericon_claim`, `sericon_release`, `sericon_baud`, and
`sericon_rescan`. Reads make prior context accessible in bounded pages. A model
retrieves it through tool calls; attachment alone does not trigger continuous
model attention. Local shell-capable agents can also use the CLI directly.

Nine additional tools expose formulas: `sericon_formulas`, `sericon_formula_api`,
`sericon_formula_show`, `sericon_formula_put`, `sericon_formula_validate`,
`sericon_formula_run`, `sericon_formula_runs`, `sericon_formula_read`, and
`sericon_formula_cancel`. An AI can inspect the API, author and validate a
formula, save it, launch a run, and retrieve its results and artifact paths.
Long UART interactions execute locally inside Sericon. Restart an existing MCP
bridge after updating to expose the new tools.

Seven Files tools bring the MCP total to twenty-four: `sericon_files_probe`,
`sericon_files_list`, `sericon_files_download`, `sericon_files_helper_install`,
`sericon_files_upload`, `sericon_files_inspect`, and `sericon_files_collect_overview`.
They return background runs
managed through the formula read/runs/cancel tools. See [Files](files.md).

## Session discovery

The control socket lives in a private directory under `$XDG_RUNTIME_DIR`.
When a client omits that variable, Sericon also checks the user's private
`/run/user/<uid>` directory before falling back to `/tmp/sericon-<uid>`.
This lets MCP clients with a reduced environment find normal terminal sessions.
`SERICON_RUNTIME_DIR` can select another private directory; pass the same
override to clients and the broker. A nonstandard `XDG_RUNTIME_DIR` must also
be passed to clients. All clients run as the same user. There is no network
listener.
