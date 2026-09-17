# Install Sericon

The beta targets Linux. Host builds cover x86-64, ARM64 and ARMv6 hard-float, including compatible Raspberry Pi systems. macOS and Windows are not supported in this release.

The source is available at [digitalandrew/sericon](https://github.com/digitalandrew/sericon) under the MIT license. Public binary download links will be added when release artifacts are published.

## Build the terminal

Install Git, Rust through rustup and the native build tools for the host distribution. Clone the source and build; `rust-toolchain.toml` selects the pinned Rust version:

```sh
git clone https://github.com/digitalandrew/sericon.git
cd sericon
cargo build --release --locked
install -Dm755 target/release/sericon "$HOME/.local/bin/sericon"
sericon --version
```

Ensure `~/.local/bin` is on `PATH`. Once built, the terminal does not require Rust, Python, an AI provider or the Athanor suite to run. The native build uses the build host's system libc; the ARM cross-builds below use static musl.

Without prepared DUT helper payloads, Cargo prints a warning and builds a terminal with shell-based Files operations. The optional helper installation menu only lists payloads compiled into the binary.

## Include all DUT helpers

On an x86-64 Linux build host, prepare the C cross-compilers described in [helper builds](helper.md#building), then run:

```sh
python3 helper/build.py
cargo build --release --locked
target/release/sericon files helpers
install -Dm755 target/release/sericon "$HOME/.local/bin/sericon"
```

The default bundle contains x86_64, mipsel, aarch64 and arm. Each Sericon host build embeds the same prepared payloads. A host's architecture does not determine the architecture of the device connected to its UART.

## Build for Raspberry Pi

From the source checkout on the build host:

```sh
sh scripts/build-pi.sh
```

| Target host | Executable |
| --- | --- |
| 64-bit Raspberry Pi OS / ARM64 Linux | `target/aarch64-unknown-linux-musl/release/sericon` |
| Compatible 32-bit hard-float OS, including original Pi/Zero | `target/arm-unknown-linux-musleabihf/release/sericon` |

Copy the appropriate executable to the Pi and install it as `~/.local/bin/sericon`. Build the DUT payloads first if the Pi should offer helper installation. The host binaries have CPU-emulation coverage; [physical hardware coverage](beta.md#validation) is recorded separately.

## Serial-device access

The account running Sericon needs read/write access to the serial device. Inspect the device's owner and group with `ls -l /dev/ttyUSB0`. Configure access using the distribution's serial-device group or udev rules. After changing group membership, start a new login session.

Run the terminal and AI clients as the same user. Running Sericon under sudo creates a separate session environment that ordinary clients may not find.

## Update an existing installation

Install the new executable, then start a new session to use its broker, formulas and bundled DUT helpers. **Ctrl-] q** stops an existing session; running `sericon` starts another with a new capture directory.

For an update limited to terminal presentation, **Ctrl-] d** followed by `sericon attach` loads the new client while keeping capture running. Reattaching does not replace the broker. Restart MCP bridges after updating to refresh their tools.
