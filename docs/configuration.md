# Configuration

The optional file is `$XDG_CONFIG_HOME/sericon/config.toml`, normally
`~/.config/sericon/config.toml`. Use `--config FILE` for an explicit file.
There is no required setup step and no automatic project configuration loading.

```toml
[adapters]
prefer = ["cp210x", "tigard"]
# serial_number = "0001"

[serial]
baud = "auto"
baud_rates = [115200, 57600, 38400, 19200, 9600, 230400, 460800, 921600]

[logging]
enabled = true
directory = "."

[terminal]
scrollback_lines = 10000
mouse = true
```

`sericon config` prints all defaults, including framing, flow control, and
detection sample settings. The [complete example](../examples/config.toml) includes every setting. Invalid settings and unknown keys produce errors.

## Connection and overrides

```sh
sericon
sericon --port /dev/ttyUSB1
sericon --baud 115200
sericon --port /dev/ttyUSB1 --baud 115200
sericon --log-dir ./captures
sericon --no-log
sericon --serial-number 0001
sericon devices
```

Each option overrides its own setting. Precedence is command-line flags, user
configuration, then built-in defaults. A fixed `--baud` skips detection;
`--baud auto` enables it even when configuration selects a fixed rate. `--log`
enables logging when configuration disables it. Relative log destinations are
resolved against the directory where the session starts.

Default adapter preference is CP210x, Tigard, single-port FTDI, CH34x, PL2303,
then CDC ACM. Tigard's second interface is excluded from automatic selection.
Within the same preference, devices are sorted by USB serial number, stable
device link, and device path. Pin an adapter with `--serial-number` or `--port`
when several matching devices are attached. Sericon reports an occupied port;
it does not displace picocom, Azoth, or another session.

The default baud candidates are 115200, 57600, 38400, 19200, 9600, 230400,
460800, and 921600. Detection is passive and looks for enough readable ASCII
in both halves of a sample, allowing whitespace and ANSI escape bytes. Silence
leaves the initial rate unconfirmed. Garbled traffic starts a bounded scan;
failure restores the first rate and reports an inconclusive result. Transmitting
input pauses automatic rate changes until an explicit rescan.

Text scoring is a heuristic, not measurement of the electrical signal. Short
prompts, binary protocols, and devices which only speak during boot can need a
fixed baud or another boot capture. Sericon sends no probe characters and does
not power-cycle targets. Linux drivers may change modem-control lines when a
port opens; DTR/RTS are configurable, with deasserted defaults.
