# Install Sericon

The beta targets Linux. Host builds cover x86-64, ARM64 and ARMv6 hard-float, including compatible Raspberry Pi systems. macOS and Windows are not supported in this release.

The source is available at [digitalandrew/sericon](https://github.com/digitalandrew/sericon) under the MIT license.

## Quick install

For Bash, Zsh or another POSIX-compatible shell on Linux:

```sh
curl -fsSL https://sericon.xyz/install.sh | sh && export PATH="$HOME/.local/bin:$PATH"
```

Then run `sericon`. The [installer script](../scripts/install.sh) selects x86-64, ARM64 or ARMv6 hard-float, downloads the 0.1.0 beta, checks its pinned SHA-256 and verifies that the executable runs before installing it. It needs `curl`, `tar`, `sha256sum` and standard Linux shell utilities. No sudo is needed.

The executable goes in `~/.local/bin`. The complete release archive, including examples and license notices, is kept in `~/.local/share/sericon`. Re-running the command updates the executable without interrupting an existing broker; [start a new session](#update-an-existing-installation) to use the new version.

The export updates PATH in your **current shell**. Many distributions already add `~/.local/bin` for future logins. If yours does not, add this line to `~/.bashrc` for Bash or `~/.zshrc` for Zsh:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

The installer leaves shell startup files unchanged. In Fish, run `curl -fsSL https://sericon.xyz/install.sh | sh`, then `fish_add_path "$HOME/.local/bin"`.

To inspect the script first or choose a different prefix:

```sh
curl -fsSLo install.sh https://sericon.xyz/install.sh
sh install.sh --help
sh install.sh --prefix /your/chosen/prefix
```

A custom prefix receives `bin/sericon` and `share/sericon/`; add its `bin` directory to PATH. See [serial-device access](#serial-device-access) if the terminal cannot open your adapter.

## Download a prebuilt release

The [0.1.0 beta release](https://github.com/digitalandrew/sericon/releases/tag/v0.1.0) includes these static Linux executables. Each embeds all four optional DUT helpers: x86_64, mipsel, aarch64 and arm. No Rust toolchain, Python runtime or shared-library installation is needed to run them.

Choose the architecture of the **host running Sericon**. Run `uname -m` on that host if unsure:

| Host / `uname -m` | Archive |
| --- | --- |
| Intel / AMD 64-bit, `x86_64` | [sericon-0.1.0-linux-x86_64.tar.gz](https://github.com/digitalandrew/sericon/releases/download/v0.1.0/sericon-0.1.0-linux-x86_64.tar.gz) |
| ARM64, `aarch64`, including 64-bit Raspberry Pi OS | [sericon-0.1.0-linux-aarch64.tar.gz](https://github.com/digitalandrew/sericon/releases/download/v0.1.0/sericon-0.1.0-linux-aarch64.tar.gz) |
| ARMv6 or later with hard-float support, `armv6l` / `armv7l`, including 32-bit Raspberry Pi OS | [sericon-0.1.0-linux-armv6hf.tar.gz](https://github.com/digitalandrew/sericon/releases/download/v0.1.0/sericon-0.1.0-linux-armv6hf.tar.gz) |

Select according to the operating system's architecture, even if the CPU supports a newer one. ARM host builds have CPU-emulation coverage; [physical Raspberry Pi validation remains pending](beta.md#validation).

Download the archive and [SHA256SUMS](https://github.com/digitalandrew/sericon/releases/download/v0.1.0/SHA256SUMS) into the same directory. This example installs the x86-64 build; replace the archive and extracted directory names for ARM:

```sh
curl -fLO https://github.com/digitalandrew/sericon/releases/download/v0.1.0/sericon-0.1.0-linux-x86_64.tar.gz
curl -fLO https://github.com/digitalandrew/sericon/releases/download/v0.1.0/SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
tar -xzf sericon-0.1.0-linux-x86_64.tar.gz
install -Dm755 sericon-0.1.0-linux-x86_64/sericon "$HOME/.local/bin/sericon"
"$HOME/.local/bin/sericon" --version
```

Continue with extraction only when the checksum reports `OK`. Put `~/.local/bin` on `PATH`, arrange [serial-device access](#serial-device-access), then run `sericon`.

Each archive also includes example configuration and a formula, license notices, dependency source archives where required, and `BUILD-INFO.json` with the source commit and binary/helper hashes. Keep these notices and sources with redistributed copies.

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
