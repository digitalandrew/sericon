# Troubleshooting

## No adapter or permission denied

Run `sericon devices` and check the expected port with `ls -l /dev/ttyUSB0`. Grant the current user access through the distribution's serial-device group or udev rules. A new login session is needed after a group change.

Use `--port /dev/ttyUSB0` or `--serial-number SERIAL` when automatic selection picks another adapter. Sericon reports an occupied port and does not displace another terminal or session.

## Waiting for baud, silence or garbled text

Sericon starts at 115200 and scores readable output. Silence leaves that rate unconfirmed. A short prompt may not provide enough evidence, and binary protocols are outside the text heuristic.

If the rate is known, start with `sericon --baud 115200`. Otherwise capture a fresh boot and use **Menu → Rescan baud rate**. Release reserved input before rescanning. Typing pauses automatic rate changes until an explicit rescan. Sericon does not reset or power-cycle the device.

## Input is busy

A human partial line, held input or interactive formula may own the writer. Finish the line or use **Menu → Yield input** at a known boundary. **Menu → Take input ownership** (Ctrl-] t) revokes the previous writer and allows recovery.

Taking over cannot undo bytes already sent. Inspect the live prompt before continuing. Failed or cancelled interactions may retain ownership because the DUT is still receiving an unfinished command. See [shared input](terminal.md#keystrokes-and-shared-input).

## Ctrl-C does not exit

Ctrl-C is forwarded to the DUT. Use **Ctrl-] q** to stop Sericon, or **Ctrl-] d** to detach while capture continues. Press Ctrl-], release, then press the second key.

## Scrolling or text selection behaves differently

Sericon scrolls within its UART pane. Page Up / Page Down work without a copy mode. Hold Shift while selecting text if the host terminal requires it for mouse selection.

Set `[terminal].mouse = false` to retain the terminal's normal mouse handling. Page Up / Page Down still work. The rendered scrollback buffer and exact session history are separate; use search to reach older retained history.

## A formula found nothing

The `passwords` and `endpoints` formulas scan retained UART history, not arbitrary host files or downloaded flash images. Run the formula again after new output arrives. With logging disabled, old history may have been evicted.

Endpoint results are candidates. Values such as `4.3.0.0` can be version numbers, and passwords may be missed when they do not match the built-in patterns. Inspect evidence context before treating a result as a finding.

## Files cannot connect

Start at an empty, logged-in Linux shell prompt. Sericon does not log in automatically or treat a boot banner as proof that a shell is ready. The shell-only backend needs the [listed transfer utilities](files.md#requirements-and-limits).

Use **Set up helper** when those utilities are missing. The helper still needs a suitable shell, bootstrap commands and a writable filesystem that permits execution. Selecting a helper path does not prove that the executable exists or matches the DUT.

## The helper menu is missing an architecture

Run `sericon files helpers` to inspect the installed executable's payloads. The menu reads the running broker's inventory. An old broker can show fewer choices even after a new executable is installed.

Stop with **Ctrl-] q**, then start `sericon` again. Detach/reattach alone does not refresh the broker. If the installed executable also lacks the payload, follow [helper builds](helper.md#building).

## Helper installation or execution fails

Match the DUT's architecture, byte order and ABI [Application Binary Interface]. The `arm` helper requires ARMv6KZ-compatible hard-float/VFPv2 support; `aarch64` targets 64-bit Linux. Also check that the destination exists, is writable and permits execution.

Only a verified, successfully executed helper becomes selected. A failed installation preserves the previous selection and reports its private DUT directory for inspection. Do not assume an upload succeeded just because some bytes were transferred.

## Downloads fail amid console noise

Sericon validates bounded chunks and retries failed exchanges. Sustained noise, a changing source file, or a shell that never returns a usable boundary can still cause failure. Inspect the job's error and `.partial` output; completed downloads are marked verified.

See [console noise and integrity](files.md#console-noise-and-integrity) for the retry, cancellation and input-recovery rules.

## An AI client cannot find a terminal session

Run both under the same user. If `SERICON_RUNTIME_DIR` or a custom `XDG_RUNTIME_DIR` is set, pass the same value to the broker and client. The normal runtime-directory fallback is described in [AI tools and MCP](ai-tools.md#session-discovery).

Restart the MCP bridge after updating Sericon. Registering a bridge does not start a UART session or automatically stream all history into a model; the client must list sessions and read the desired one.
