# Beta scope and validation

This documentation covers the Sericon 0.1.0 beta. The beta includes Linux serial sessions, passive adapter/baud selection, RX/TX logging, scrollback, literal/regex search, Rhai formulas, local CLI/MCP access, and embedded Linux file operations.

## Platform scope

| Component | Included targets |
| --- | --- |
| Sericon host | Static Linux x86-64, ARM64 and ARMv6 hard-float |
| Optional DUT helper | x86_64, mipsel, aarch64, arm (ARMv6KZ hard-float/VFPv2) |
| Other host operating systems | Not included |

Host and DUT architectures are independent. A Linux x86-64 laptop can install the MIPS or ARM helper on a connected device. The helper is optional; normal serial use needs no software installed on the DUT.

## Limits

- Baud detection uses readable-text scoring. Silence, brief prompts and binary traffic may need a fixed rate or another capture.
- Disconnecting ends a session. Automatic reconnect and terminal replay of a closed capture are not included.
- Human and agent input is coordinated through writer reservations. It does not establish when a remote command has finished.
- Files requires an empty, logged-in Linux shell. Regular-file transfers are limited to 64 MiB, and a directory listing to 512 entries. Device/flash reads, shell-only uploads and persistent transfer resume are not included.
- Helper architecture selection is manual. Matching a CPU family alone does not establish the required byte order, ABI or kernel compatibility.
- The built-in password and endpoint scans return candidates with evidence context. They do not establish that a value is a working credential or reachable endpoint.
- Athanor integration points exist through the CLI and MCP. The Athanor GUI does not yet control Sericon sessions.

The [Files guide](files.md#requirements-and-limits), [search guide](search.md) and [formula API](formula-api.txt) document the operation-specific limits.

## Validation

As of September 17 2026:

| Coverage | Verified |
| --- | --- |
| Native software fixtures | 36 Rust tests and 73 isolated PTY [pseudoterminal] integration tests; formatting, Clippy and locked builds |
| Published host packages | Static musl executables for all three targets, with all four helper payloads; versions, embedded hashes, archive checksums and dependency notices checked |
| Physical CP2102 / ESP32 | Adapter discovery, 115200 baud recognition, shared session I/O and retained logging on a Hatch Rest |
| Physical Tigard / TP-Link MT7628 | Adapter selection, fixed 115200 operation, target-specific U-Boot interruption, and protocol-1 helper installation with verified binary downloads |
| C helper CPU tests | x86-64 natively; MIPS24Kc, ARM Cortex-A53 and ARM1176 under QEMU. Reads, listings, inspection, verified uploads, corruption/collision handling and unsafe-path rejection |
| ARM Sericon host CPU tests | ARM64 and ARMv6 broker startup, PTY I/O, formulas, downloads, helper upload/overview collection, and clean shutdown under QEMU |
| Helper menu | All four payloads selectable, Back preserves selection, installation verifies and selects the helper |

Emulation is separate from physical hardware testing. ARM DUT helpers and Raspberry Pi hosts have not yet received physical validation. The current protocol-2 helper operations have software/CPU coverage; earlier physical helper downloads used protocol 1. This is not a compatibility claim for every adapter, old kernel or embedded Linux image.

## Report a beta issue

Open an issue in [digitalandrew/sericon](https://github.com/digitalandrew/sericon/issues).

Include the output of `sericon --version`, host OS and architecture, adapter model, selected port/baud, and steps to reproduce. For helper problems, include `sericon files helpers` and the DUT's known architecture, kernel and shell. State whether the problem occurs in the terminal, CLI or MCP bridge.

Attach the relevant error and a short capture excerpt after removing credentials or other private DUT data. Note whether logging was disabled and whether other clients or formulas were active. A reproducible PTY fixture is useful when hardware is unavailable.
