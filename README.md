# Sericon

A serial terminal for Linux, built for shared sessions with people and local AI tools.

```sh
sericon
```

Sericon selects an attached UART [Universal Asynchronous Receiver/Transmitter] adapter, starts at 115200 baud, and checks incoming text to identify the rate. Received and transmitted bytes are logged in the current directory. A human terminal, scripts and AI coding tools can use the same running session and its retained history.

**0.1.0 beta · Linux, including Raspberry Pi**

[Get started](docs/getting-started.md) · [Install](docs/installation.md) · [Documentation](docs/index.md) · [Beta scope](docs/beta.md) · [MIT license](LICENSE)

[![Sericon action menu over a live UART session](docs/assets/screenshots/main-menu.png)](docs/assets/screenshots/main-menu.png)

## What it does

- Selects USB adapters by preference, with port, serial-number and baud overrides.
- Logs both directions and keeps capture running when the terminal detaches.
- Scrolls within the UART pane and searches retained history with literal text or regex.
- Runs embedded Rhai scripts, called **formulas**, for history scans and UART interactions.
- Browses and transfers embedded Linux files over serial, with checksums and retries for console noise.
- Shares the session through the CLI [Command-Line Interface] and MCP [Model Context Protocol], including formula authoring and execution.

Sericon runs independently of Athanor. No AI provider, model key, Python runtime or network connection is required for the terminal. AI access is optional.

## Install and start

Clone the source and build with the Rust toolchain pinned in `rust-toolchain.toml`:

```sh
git clone https://github.com/digitalandrew/sericon.git
cd sericon
cargo build --release --locked
install -Dm755 target/release/sericon "$HOME/.local/bin/sericon"
sericon
```

Put `~/.local/bin` on `PATH` and give the current user access to the serial device. This builds the terminal; [installation](docs/installation.md) also covers Raspberry Pi builds and bundling the optional DUT helpers.

Common overrides:

```sh
sericon --port /dev/ttyUSB0 --baud 115200
sericon --log-dir ./captures
sericon --no-log
```

Automatic baud detection needs enough readable traffic. Silence leaves the initial rate unconfirmed. Sericon sends no probe characters and does not reset the device. See [configuration](docs/configuration.md) for adapter preferences, baud candidates and logging defaults.

## Find the controls

Press **Ctrl-]**, release, then **m** to open the menu. Arrows select, Enter chooses, and Esc/q goes back. Every option menu uses these controls.

| Task | Control |
| --- | --- |
| Scroll earlier output | Mouse wheel or Page Up / Page Down |
| Search history | Ctrl-] f |
| Run a formula | Ctrl-] x |
| Embedded Linux / Files | Ctrl-] l |
| Return to live output | Ctrl-] b |
| Help | Ctrl-] ? |
| Detach; keep capture running | Ctrl-] d |
| Stop; flush logs and close UART | Ctrl-] q |

**Ctrl-C goes to the device.** To reconnect after detaching, run `sericon attach`. Use `sericon sessions` and an explicit session ID when several are running.

## Embedded Linux over serial

At an empty, logged-in Linux shell prompt, open **Embedded Linux / Files → Connect and browse**. Download files using the DUT's existing shell tools, or install the optional static C helper from **Set up helper**.

The helper adds an expandable file tree, verified uploads, device inspection and saved overview collections. Bundled payloads cover x86_64, mipsel, aarch64 and arm [ARMv6KZ hard-float/VFPv2]. Select the DUT architecture independently of the host running Sericon.

[![Helper-backed file tree, with Enter download shown for a selected file](docs/assets/screenshots/file-tree.png)](docs/assets/screenshots/file-tree.png)

See [Files](docs/files.md) for requirements and transfer limits, or [helper setup](docs/helper.md) for the illustrated installation steps.

## Share with an AI coding tool

Register the local bridge with the client in use:

```sh
codex mcp add sericon -- "$HOME/.local/bin/sericon" mcp
claude mcp add --transport stdio --scope user sericon -- "$HOME/.local/bin/sericon" mcp
```

Start `sericon` in a terminal, then ask the client to list Sericon sessions and read the desired session. Both clients can see the traffic; writer reservations coordinate input. An agent can also create and run formulas for repeated or time-sensitive UART work.

[AI tools and MCP](docs/ai-tools.md) covers session discovery, context, input ownership and the available tools. [Formulas](docs/formulas.md) covers built-in scans and custom scripts.

## Documentation and beta feedback

Start with the [documentation index](docs/index.md). The planned documentation domain is **sericon.xyz**; the guides are available in this checkout now.

Report problems in [GitHub Issues](https://github.com/digitalandrew/sericon/issues). The beta targets Linux. Baud detection is a text heuristic; file transfers require a ready Linux shell; automatic reconnect is not included. [Beta scope and validation](docs/beta.md) distinguishes software fixtures, CPU emulation and physical hardware coverage, and lists the details to include in an issue report.

For source checks, documentation preview and release preparation, see [development](docs/development.md).

## License

Sericon is licensed under the [MIT license](LICENSE). See [third-party components](docs/third-party.md) for dependency notices.
