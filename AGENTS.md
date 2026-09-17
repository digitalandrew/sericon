# Sericon contributor instructions

Read README.md and docs/development.md first, plus docs/session-handoff.md when
present in a maintainer checkout. Sericon is a standalone Rust serial terminal;
Athanor is a separate project.

Preserve the no-argument workflow: discover an adapter, try 115200 first, display
traffic immediately, and log in the launch directory. Explicit flags override
configuration; configuration overrides defaults.

The session broker owns the port. Terminal, CLI, MCP, and future integrations
must share it. Preserve exact RX/TX bytes, writer coordination, cursor history,
logging-off behavior, and terminal restoration.

All TUI option menus must use the shared `src/picker.rs` component and the
Ctrl-] m style: highlighted rows, arrows/wheel to move, Enter to choose, and
Esc/q plus a visible Back row to return. Shortcuts are optional conveniences;
never require memorising letters for an action. Use prompts only for text or
numeric entry, preserve the previous view on return, and keep menu input local.
Routine formula configuration uses individual fields; JSON is an advanced option.

Use the pinned Rust toolchain. Run `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, `cargo test --locked`,
`cargo build --locked`, and `python3 tests/integration.py` for relevant code
changes. Use isolated PTYs; do not open physical adapters incidentally.

Keep build output and captures out of source. Check existing work before edits.
When a local docs/session-handoff.md is present, update it with implementation
and validation changes. Distinguish fixtures and CPU emulation from physical
UART tests. Local handoffs, hardware records and original captures are ignored
by Git; public coverage belongs in docs/beta.md.

Public guides live in docs/ and build with mkdocs.yml; the handoff, requirements
and detailed hardware notes are excluded from the site. For documentation-only
changes, run the strict MkDocs build and scripts/check-docs.py in the environment
from requirements-docs.txt. Check layout/navigation changes at desktop and mobile
widths. Preserve original screenshots; use unmodified named copies in docs/assets.
