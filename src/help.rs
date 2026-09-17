//! Shared user-facing reference for CLI and interactive help.

pub const LIVE: &str = "Live terminal (press Ctrl-], release, then the next key):
  m  Open the action menu (also from search, formulas and help)
  ?  Show help                   f  Search retained RX/TX history
  q  Stop and close the UART     d  Detach; keep capture running
  a  Yield input ownership       t  Take input ownership
  o  Toggle holding input        r  Rescan baud; release input first
  ]  Send a literal Ctrl-]        b  Return to latest live output
  x  Open formulas and background runs
  l  Embedded Linux / Files (ready Linux shell required)
Ctrl-C goes to the device. Partial input stays reserved until Enter,
Ctrl-C or Ctrl-D; hold mode keeps ownership across those boundaries.

Scrollback (no prefix or copy mode needed):
  Wheel over UART output: 3 lines   Page Up/Down: a page
  End while scrolled / Ctrl-] b: live   Typing: live and send to DUT
  q stays a normal UART character; End passes through at the live prompt.
  New output, capture and other clients continue while you read older lines.
  Default: 10,000 rendered lines per terminal, separate from session history.
  Config [terminal]: scrollback_lines = 10000 (0..100000), mouse = true.
  mouse = false keeps normal mouse handling; Page Up/Down still scroll.
  Text selection may need Shift-drag, depending on your terminal.";

pub const MENU: &str = "Action menu (Ctrl-] m from any view):
  Up/Down, j/k or wheel: select   Enter: choose   Esc/q: close
  Closing restores the previous view, including its query or formula options.
  Detach keeps capture running; Quit / stop session closes the UART.
  Menu keys stay local. Capture and other clients continue in the background.";

pub const SEARCH: &str = "Search prompt:
  Enter: search   Tab: literal/regex   Backspace: edit   Ctrl-U: clear
  q, f and r are ordinary letters while typing a query.
Search results (also while loading or after no matches):
  Enter: next match, wrapping at the end   q: return to live
  f: fresh query in the same mode   r/Tab: switch mode, keep query
In either search view:
  Esc: options menu for query, next match, literal/regex mode, help and return.
  Options use arrows/wheel, Enter to choose, Esc/q to go back.
  Ctrl-] b: return to live   Ctrl-] f: fresh query   Ctrl-] ?: help
  Ctrl-] q: stop the session   Ctrl-] d: detach
Regex examples: error|warning   (?i)failed   ^ERROR\\b   code=\\d{3}
Regex matches one line at a time; no look-around or backreferences.
Queries: up to 1,024 UTF-8 bytes. Regex lines: up to 8 MiB.
Invalid patterns stay editable; a highlighted bar marks zero-width matches.
Search keys stay local. Capture and other clients continue during search.
Each search uses retained history; f refreshes it. No-log gaps are reported.";

pub fn terminal() -> String {
    format!("{LIVE}\n\n{MENU}\n\n{FILES}\n\n{FORMULAS}\n\n{SEARCH}")
}

pub const FILES: &str = "Embedded Linux / Files (Ctrl-] l, or action menu):
  Every option menu uses arrows/j/k or the wheel to select, Enter to choose,
  and Esc/q to go back. Home/End selects the first/last row. Shortcuts are optional.
  Start at an empty, logged-in Linux shell; choose Connect and browse.
  Set up helper opens install/upgrade, existing-path and shell-tools choices.
  Helper browser: a cached file tree with folder/file-type icons and colours.
  Right/l expands or enters folders; Left/h/Backspace collapses or selects parent.
  Enter toggles folders; Enter download on files opens the host location menu.
  Tree options (o/Tab) offers Scan filesystem, Refresh selected folder and returns.
  Scan caches below the current root; skips /proc, /sys, /dev and symlink targets.
  Stop scanning/c finishes the current directory then stops. Folders also load on open.
  Cache: 16,384 entries, 512 per folder, depth 32; scans pause after 1,024 folders.
  Shell-tools browser: Enter opens directories; Parent directory and return rows.
  Use ./downloads (default) starts downloading beneath the current host directory.
  Enter another directory opens an empty boxed field; type a host path, then Enter.
  Esc cancels entry; Back restores the browser selection. Each download has its own folder.
  Shortcuts: Backspace: parent in flat browser; collapse/parent in tree
  g: directory path   d: file path   h: helper path   r: recent job   c: cancel job
  a: install/upgrade helper. Choose DUT architecture/ABI, then the install location.
  Use /var/tmp or Enter another directory: type a DUT path in the empty field.
  The folder must allow writing and execution. Enter reviews; Install helper starts.
  A verified successful install selects its helper automatically in Files.
  Current helper: u uploads a host file, i opens the device inspector,
  s collects an overview and hash manifest in a chosen host directory.
  Upload: source, new DUT path, then select Upload file. The permission row toggles
  execution permission with Enter; e remains a shortcut.
  Existing destinations are never replaced; uploaded files are never executed.
  Inspector: choose a section, then arrows scroll; Enter/Esc returns to sections.
  Job screen: Enter opens progress/results, cancel and return options.
  v views collected sections from a completed job. Missing sections are reported.
  Esc/q: previous menu   Ctrl-] m: main menu   Ctrl-] b: live (job continues)
  Path/destination prompts: Ctrl-U clears, Esc/Ctrl-C goes back, Enter continues.
  DUT needs dd, base64/od/hexdump, and cksum (or sha256sum plus wc). Browsing
  also needs printf. Or select an uploaded static helper with h / --helper.
  Install from Files with a, or files helper-install --arch ARCH --directory DUT_DIR.
  The picker lists the running broker's payloads; older sessions need a restart.
  files helpers lists bundled architectures; linux-helper-install is a formula.
  Each chunk is verified and retried on noise; whole-file verification follows.
  Output: unique formula-RUN directory; failures keep NAME.partial.
  Limits: stable regular files <=64 MiB; directories <=512 entries.
  During an incomplete exchange cancellation may retain input for recovery.
  CLI: files probe | list PATH | download PATH --output-dir DIR | read RUN
  CLI: files upload SOURCE DUT_PATH --helper PATH [--executable]
       files inspect --helper PATH
       files collect-overview --helper PATH --output-dir DIR
  All Files jobs also appear under formulas runs; see docs/files.md.";

pub const FORMULAS: &str = "Formulas (Ctrl-] x):
  All option menus match Ctrl-] m: arrows/j/k or wheel selects, Enter chooses,
  Esc/q goes back. Home/End selects the first/last row; Ctrl-] b returns live.
  Choose a formula, edit individual settings, then choose Run formula.
  Boolean rows toggle with Enter. Text/number prompts: Enter saves, Esc cancels.
  Advanced JSON remains available as a menu choice; Enter there starts the run.
  Recent runs, help, source, cancellation, result pages and details have menu rows.
  Optional shortcuts:
  f: formula list   r: recent runs   s: source   c: cancel selected run
  n/p: result pages   q: return   Ctrl-] b: live   Ctrl-] t: take over input
  v: full run status/artifacts   j/k scroll source and finding context
  ?: help   Ctrl-] ?: help from any formula screen (including options)
  Ctrl-U clears a text/number/JSON prompt.
  Leaving the view keeps background jobs running. Interactive formulas reserve
  input; human takeover cancels their writes. See 'sericon formulas api'.";

pub const FORMULA_CLI: &str =
    "Built-ins: passwords, endpoints, linux-probe, linux-list, linux-download,
linux-helper-install (explicit helper installation), linux-upload (source, path,
helper, optional executable), linux-inspect (helper), linux-collect-overview
(helper and explicit output_dir).
Formulas use embedded Rhai; no interpreter or grep executable is needed.
Run against an existing session; omit --session when exactly one exists.
Runs return an ID immediately. Read results with 'read RUN', resuming next_cursor.
Interactive formulas can send UART input. Inspect source with 'show NAME'.
Custom formulas: [[formulas]] config entries, or 'put NAME --file script.rhai'.

Examples:
  sericon formulas list
  sericon formulas run endpoints
  sericon formulas read RUN --session SESSION
  sericon formulas put my-script --file script.rhai --interactive
  sericon formulas validate my-script
  sericon formulas run my-script --output-dir ./exports
  sericon formulas cancel RUN
  sericon formulas api";

pub fn command_line() -> String {
    format!(
        "Bare sericon selects an adapter, tries 115200 first, and logs RX/TX in
the launch directory. Detection is passive; silence stays unconfirmed.
Connection/logging flags apply to new sessions, not existing ones.

{}

Examples:
  sericon --port /dev/ttyUSB0 --baud 115200
  sericon --log-dir ./captures
  sericon --no-log
  sericon --detach
  sericon sessions
  sericon attach
  sericon formulas run endpoints
  sericon mcp

Configuration: $XDG_CONFIG_HOME/sericon/config.toml
  (normally ~/.config/sericon/config.toml); --config selects another file.
Flags override config, which overrides defaults; 'sericon config' prints defaults.
Use 'sericon <command> --help' for scripting and MCP details.",
        terminal()
    )
}

pub const READ: &str = "Start with --after 0 for retained history. Continue with next_cursor;
has_more means another page is available, history_gap reports evicted history.
Logged sessions can read older journal data; --no-log retains bounded memory.
Reading transmits nothing. Omit SESSION when exactly one session is running.

Examples:
  sericon read SESSION --after 0 --limit 128
  sericon read SESSION --after 42 --wait-ms 1000";

pub const SEND: &str = "Normal text appends carriage return and releases the CLI writer.
--raw and --hex append nothing and retain ownership until 'sericon release'
or a subsequent complete text command. The CLI shares one writer identity.
An unfinished human or agent command returns busy; input is not queued.
Sending pauses automatic baud changes until an explicit rescan.

Examples:
  sericon send SESSION 'help'
  sericon send SESSION --hex 03
  sericon release SESSION";

pub const MCP: &str = "Start a UART session first, then let a coding agent join it through this
stdio bridge. The bridge shares the existing port, history, and coordinated
input. It does not start a capture or open a second serial connection.

Tools: sericon_sessions, sericon_status, sericon_read, sericon_send,
       sericon_claim, sericon_release, sericon_baud, sericon_rescan.
Formulas: sericon_formulas, sericon_formula_api, sericon_formula_show,
          sericon_formula_put, sericon_formula_validate, sericon_formula_run,
          sericon_formula_runs, sericon_formula_read, sericon_formula_cancel.
Files: sericon_files_probe, sericon_files_list, sericon_files_download,
       sericon_files_helper_install, sericon_files_upload,
       sericon_files_inspect, sericon_files_collect_overview.
Uploads and inspection require a current helper and ready Linux shell.
Files tools return background runs; read/cancel them with the formula tools.
AI clients can author and validate Rhai source, then start and inspect runs.
Read history in pages, then continue from next_cursor. No AI API key is needed.
The bridge and session must run as the same user. Pass custom runtime-directory
overrides to both; normal Linux login directories are recovered automatically.

Registration (when sericon is on PATH):
  codex mcp add sericon -- sericon mcp
  claude mcp add --transport stdio --scope user sericon -- sericon mcp";
