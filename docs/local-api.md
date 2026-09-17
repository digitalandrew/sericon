# Local session API, version 0.1

One session process exclusively owns each serial port. Terminals, CLI commands,
and MCP bridges connect over a Unix-domain socket. No client consumes bytes
directly from another client's serial descriptor. This API is an initial local
integration surface; Athanor/Azoth GUI integration is not part of version 0.1.

Discover sessions with `sericon sessions --json`. Each session's socket is
`<runtime directory>/<12-character session ID>.sock`. The runtime directory
is owner-only (0700), and sockets are mode 0600. Use the CLI or MCP when possible
so integrations do not depend on choosing the runtime directory themselves.

Runtime directory precedence is `SERICON_RUNTIME_DIR`, `$XDG_RUNTIME_DIR/sericon`,
then `/run/user/<uid>/sericon` when its parent is private and owned by the current
user, then the temporary directory's `sericon-<uid>` child. Recovering the
standard login directory lets MCP clients omit `XDG_RUNTIME_DIR` without losing
access to a terminal's sessions. Custom paths must be supplied to all clients.

Each connection carries one newline-delimited JSON request and response:

```json
{"op":"read","after":0,"limit":32,"wait_ms":1000}
```

Responses contain either `{"result": ...}` or `{"error": "description"}`.
Status and read requests do not transmit. Mutations are serialized by the
session's serial I/O loop. Queued requests expire before the client timeout;
a transport failure after transmission can still have an unknown outcome.
Do not blindly retry a write after a lost response.

| Operation | Fields | Behavior |
| --- | --- | --- |
| `status` | None | Current adapter, baud, detection, logging, writer, and cursor |
| `read` | `after`, `limit`, `wait_ms` | Ordered events after the cursor; limit 1..128, wait 0..30000 ms |
| `send` | `data_base64`, `actor`, `client_id`, `release` | Write 1..4096 bytes; optionally release input afterward |
| `claim` | `actor`, `client_id`, `takeover` | Reserve input; explicit takeover is used by the human terminal |
| `release` | `client_id` | Release that client's reservation |
| `baud` | `rate` | Set fixed baud; requires no reserved writer |
| `rescan` | None | Restart passive scan; requires no reserved writer |
| `stop` | None | Begin stopping and flushing; serial port closes in the broker loop |
| `formula_start` | `formula`, `params`, `initiator`, `timeout_ms`, `output_dir` | Validate resolved source/parameters and start background work; returns a run ID |
| `formula_runs` | None | Recent runs, progress, status, and artifact metadata |
| `helper_inventory` | None | Running broker's bundled DUT payloads: architecture, ABI, protocol, byte size and SHA-256; no UART access |
| `formula_read` | `run`, `after`, `limit` | Status and paged results; `after` is a result index, independent of the UART event cursor |
| `formula_cancel` | `run` | Cancel the run and revoke further formula writes |

Actor/client names contain 1..64 ASCII letters, digits, periods, underscores,
colons, or hyphens. They identify writers within the local user's session;
they are not identities authenticated independently of that Unix user.
The `formula-` client prefix is reserved for registered interactive runs. Human
takeover revokes that run, so it cannot reacquire input after a later release.
Formula lifecycle/status work runs separately from the serial I/O loop; writes
still go through its coordinated queue. An in-flight write may finish during
cancellation. FormulaStart accepts inline `source`, never a broker-side script
path. Clients resolve definitions through their selected config/library first.

Formulas use a default 300000 ms deadline, capped at 86400000 ms. `output_dir`
must be an absolute path if supplied. Definitions and typed parameters are
documented in [the formula guide](formulas.md); the full embedded runtime API is
available through `sericon formulas api` and `sericon_formula_api` over MCP.

Read results include `events`, `next_cursor`, `latest_cursor`, `oldest_available`,
`history_gap`, and `has_more`. Start at zero for history and advance using
`next_cursor`. Cursors belong to one session. The byte encoding in an event's
`data_base64` field is authoritative; `text` can contain replacement characters
when binary or split UTF-8 bytes are decoded. Timestamps describe host handling
of I/O chunks, not individual electrical transitions.

The in-memory history is bounded. Logged sessions retrieve older events from
their journal with a sparse file index. Sessions with logging disabled report
an explicit history gap when older data has been evicted. A slow reader does
not own the serial port or hold up another client's cursor.

The daemon survives terminal detach. Stopping or unplugging ends it, leaves
the final status available briefly, and removes its socket. Captures survive.
There is no automatic reconnect in this build, so a newly attached adapter
cannot silently become the target of an existing writer.

The MCP bridge implements local stdio JSON-RPC with tools and initialization.
It negotiates the supported 2024-11-05, 2025-03-26, or 2025-06-18 protocol;
newer clients can use the returned 2025-06-18 version. Its stdout carries only
protocol messages. Its read tool caps each wait at five seconds.
