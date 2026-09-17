# Sericon

Sericon keeps a serial session available to a human terminal, scripts and local AI tools. Start with `sericon`: it selects an adapter, tries 115200 baud first and logs traffic in the launch directory.

**Version 0.1.0 beta · Linux, including Raspberry Pi**

[Get started](getting-started.md) · [Install](installation.md) · [Beta scope](beta.md) · [Source on GitHub](https://github.com/digitalandrew/sericon)

[![Sericon’s action menu over a live serial session](assets/screenshots/main-menu.png)](assets/screenshots/main-menu.png)

Select a screenshot to open it at full resolution.

## Choose a guide

| Task | Guide |
| --- | --- |
| Install and connect to an adapter | [Installation](installation.md), [get started](getting-started.md) |
| Navigate menus, scroll back or recover input | [Terminal controls](terminal.md) |
| Find text or regex matches in history | [Search](search.md) |
| Set adapter priorities, baud candidates or logging | [Configuration](configuration.md) |
| Detach, reconnect or work with capture files | [Sessions and logging](sessions.md) |
| Browse, download or upload DUT files | [Embedded Linux Files](files.md) |
| Install a helper on a stripped Linux image | [Optional helper](helper.md) |
| Analyze history or automate UART interactions | [Formulas](formulas.md), [formula API](formula-reference.md) |
| Connect Codex, Claude Code or another client | [AI tools and MCP](ai-tools.md) |
| Resolve connection, input or transfer issues | [Troubleshooting](troubleshooting.md) |
| Integrate with a session broker | [Local session API](local-api.md) |
| Check platform coverage and beta limits | [Beta scope and validation](beta.md) |

## Files through the same UART

The optional helper supplies directory browsing, file reads, verified uploads and device inspection without a network connection or target-side interpreter. Folders load on demand; a scan can fill the cache below a selected root.

[![Embedded Linux file tree](assets/screenshots/file-tree.png)](assets/screenshots/file-tree.png)

Source build and documentation instructions are in [development](development.md). Sericon uses the [MIT license](../LICENSE); dependency notices are listed under [third-party components](third-party.md).
