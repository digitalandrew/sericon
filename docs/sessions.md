# Sessions and logging

A background session broker owns the serial port. Terminal clients, command-line tools and AI bridges share that connection and read history independently. Detaching leaves the broker and capture running; stopping closes the port.

## Session commands

```sh
sericon --detach                   # Start without attaching; prints JSON status
sericon sessions
sericon attach SESSION
sericon status SESSION
sericon read SESSION --after 0
sericon read SESSION --after 42 --wait-ms 1000
sericon send SESSION 'help'        # Appends carriage return
sericon send SESSION --hex 03     # Raw Ctrl-C; retains the CLI writer
sericon release SESSION
sericon baud SESSION 57600
sericon rescan SESSION
sericon stop SESSION
```

Status, reads, and command results use JSON. Reads return ordered events,
`next_cursor`, `has_more`, and `history_gap`. Continue from `next_cursor` to
avoid rereading earlier output. Most session commands can omit the ID when
exactly one session exists; `send` and `baud` require it.

Text sent with `--raw` and input sent with `--hex` omit Enter and retain the
command-line writer until `sericon release SESSION` or a subsequent complete
text command. The CLI uses the shared `cli` writer identity. Each MCP bridge
and each terminal has its own writer identity.

## Logging and retained context

With logging enabled, each session creates a private directory named
`sericon-<UTC timestamp>-<session ID>` containing:

- `events.jsonl`: the authoritative ordered journal, with original bytes in
  `data_base64`, a decoded text convenience field, direction, actor, host
  timestamps, elapsed time, baud, and connection events.
- `transcript.txt`: a readable event transcript. Nonprintable bytes are escaped;
  the journal preserves the exact byte stream.

Journal writes occur as events are recorded. Files are synchronized every
second and on normal stop. Logging failures stop the session and are reported
to attached clients; startup fails if the destination cannot be created.
Transmitted bytes mean bytes accepted by the serial driver, not a device-side
execution acknowledgement. Partial writes retain input ownership and report
the number accepted; inspect history before retrying.

Active sessions serve older history from the journal when it leaves the memory
buffer. `--no-log` writes neither capture file and retains only a bounded memory
history (approximately 4 MiB of event data). Reads explicitly report an eviction
gap. Closed sessions' capture files remain readable directly; replaying them as
a terminal is not included in this beta.

## Updating a running session

Detach with **Ctrl-] d**, then run `sericon attach` to load a new terminal UI while the broker keeps running. Changes to formulas, file operations or bundled helpers require a new broker: **Ctrl-] q**, then start `sericon` again. Existing capture directories remain on disk. Restart MCP bridges after updates to refresh their tool inventory.
