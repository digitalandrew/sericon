#!/usr/bin/env python3
"""Linux PTY acceptance tests. No physical UART or existing user sessions used."""
import base64
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import socket
import subprocess
import struct
import sys
import tempfile
import termios
import time
import unittest
from file_shell import Shell, corrupt_chunk

BINARY = Path(os.environ.get("SERICON_TEST_BINARY", Path(__file__).resolve().parents[1] / "target/debug/sericon")).resolve()


def eventually(fn, timeout=5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = fn()
        if value:
            return value
        time.sleep(0.02)
    raise AssertionError("condition did not become true")


class MCP:
    def __init__(self, env):
        self.p = subprocess.Popen([str(BINARY), "mcp"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.seq = 0
        self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "fixture-agent", "version": "1"}})
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        self.p.stdin.flush()

    def rpc(self, method, params=None):
        self.seq += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.seq, "method": method, "params": params or {}}) + "\n")
        self.p.stdin.flush()
        if not select.select([self.p.stdout], [], [], 8)[0]:
            raise AssertionError("MCP response timed out")
        return json.loads(self.p.stdout.readline())

    def tool(self, tool_name, **args):
        reply = self.rpc("tools/call", {"name": tool_name, "arguments": args})
        result = reply["result"]
        content = result["content"][0]["text"]
        if result.get("isError"):
            return {"error": content}
        return json.loads(content)

    def close(self):
        self.p.stdin.close()
        try:
            self.p.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.p.kill()
            self.p.wait(timeout=3)
        self.p.stdout.close()
        self.p.stderr.close()


class SessionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="sericon-test-")
        self.root = Path(self.tmp.name)
        self.env = dict(os.environ, TERM="xterm-256color", NO_COLOR="", SERICON_RUNTIME_DIR=str(self.root / "runtime"), XDG_CONFIG_HOME=str(self.root / "config"))
        self.master, self.slave = pty.openpty()
        self.port = os.ttyname(self.slave)
        self.sessions = []
        self.agents = []
        self.terminals = []

    def tearDown(self):
        for agent in self.agents:
            agent.close()
        for session in self.sessions:
            try:
                self.control(session, {"op": "stop"})
            except (OSError, ValueError):
                pass
        for process, master, slave in self.terminals:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            os.close(master)
            os.close(slave)
        for fd in (self.master, self.slave):
            if fd is not None:
                os.close(fd)
        eventually(lambda: not list((self.root / "runtime").glob("*.sock")), timeout=5)
        self.tmp.cleanup()

    def cmd(self, *args, check=True):
        return subprocess.run([str(BINARY), *map(str, args)], env=self.env, cwd=self.root, capture_output=True, text=True, timeout=12, check=check)

    def start(self, *args):
        result = json.loads(self.cmd("--port", self.port, "--detach", *args).stdout)
        self.sessions.append(result["id"])
        return result

    def control(self, session, request):
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(5)
            stream.connect(str(self.root / "runtime" / (session + ".sock")))
            stream.sendall(json.dumps(request).encode() + b"\n")
            with stream.makefile("rb") as incoming:
                return json.loads(incoming.readline())

    def status(self, session):
        return self.control(session, {"op": "status"})["result"]

    def read(self, session, after=0):
        return self.control(session, {"op": "read", "after": after, "limit": 128})["result"]

    def input_bytes(self, expected, timeout=3):
        result = b""
        deadline = time.monotonic() + timeout
        while len(result) < len(expected) and time.monotonic() < deadline:
            if select.select([self.master], [], [], 0.1)[0]:
                result += os.read(self.master, 8192)
        self.assertEqual(result, expected)

    def agent(self):
        agent = MCP(self.env)
        self.agents.append(agent)
        return agent

    def terminal(self, *args):
        master, slave = pty.openpty()
        original = termios.tcgetattr(slave)
        p = subprocess.Popen([str(BINARY), *args], env=self.env, cwd=self.root, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        self.terminals.append((p, master, slave))
        return p, master, slave, original

    def wait_terminal(self, fd, needle):
        output = bytearray()
        def got():
            if select.select([fd], [], [], 0.02)[0]:
                output.extend(os.read(fd, 65536))
            return needle in output
        eventually(got)
        return bytes(output)

    def test_default_logging_preserves_rx_tx_bytes_and_direction(self):
        s = self.start("--baud", "115200")
        self.assertEqual(s["detection"], "fixed")
        self.assertEqual(Path(s["log_directory"]).parent, self.root)
        payload = b"boot\r\n\x00\xff\x1b[32mready\x1b[0m\r\n"
        os.write(self.master, payload)
        eventually(lambda: any(e["kind"] == "rx" for e in self.read(s["id"])["events"]))
        self.cmd("send", s["id"], "version")
        self.input_bytes(b"version\r")
        events = self.read(s["id"])["events"]
        received = b"".join(base64.b64decode(e["data_base64"]) for e in events if e["kind"] == "rx")
        sent = b"".join(base64.b64decode(e["data_base64"]) for e in events if e["kind"] == "tx")
        self.assertEqual(received, payload)
        self.assertEqual(sent, b"version\r")
        disk = [json.loads(line) for line in (Path(s["log_directory"]) / "events.jsonl").read_text().splitlines()]
        self.assertEqual(disk, events)
        self.assertEqual((Path(s["log_directory"]).stat().st_mode & 0o777), 0o700)
        self.assertEqual((Path(s["log_directory"]) / "events.jsonl").stat().st_mode & 0o777, 0o600)

    def test_config_and_flags_override_independently(self):
        config = self.root / "settings.toml"
        config.write_text('[serial]\nbaud = 9600\n[logging]\nenabled = false\ndirectory = "from-config"\n')
        s = self.start("--config", config, "--baud", "115200", "--log", "--log-dir", "from-flag")
        self.assertEqual(s["baud"], 115200)
        self.assertEqual(Path(s["log_directory"]).parent, self.root / "from-flag")

    def test_no_log_creates_no_capture_files(self):
        s = self.start("--no-log")
        os.write(self.master, b"Linux version 6.1.0 boot complete\r\nWelcome to the UART console\r\n")
        eventually(lambda: self.status(s["id"])["detection"] == "locked")
        self.assertIsNone(s["log_directory"])
        self.assertEqual([p.name for p in self.root.iterdir()], ["runtime"])

    def test_silence_remains_at_115200_without_transmitting(self):
        s = self.start("--no-log")
        time.sleep(0.9)
        state = self.status(s["id"])
        self.assertEqual(state["baud"], 115200)
        self.assertEqual(state["detection"], "waiting")
        self.assertFalse(select.select([self.master], [], [], 0.05)[0])

    def test_garbage_scans_and_text_locks(self):
        s = self.start("--no-log")
        os.write(self.master, b"\xff\x00\xfe\x80" * 32)
        eventually(lambda: self.status(s["id"])["baud"] == 57600)
        os.write(self.master, b"Linux version 6.1.0 starting\r\nSystem boot complete, login: \r\n")
        eventually(lambda: self.status(s["id"])["detection"] == "locked")
        self.assertEqual(self.status(s["id"])["baud"], 57600)

    def test_mcp_joins_history_and_shares_input(self):
        s = self.start("--no-log", "--baud", "115200")
        os.write(self.master, b"Earlier boot context before the agent joined\r\n")
        eventually(lambda: len(self.read(s["id"])["events"]) > 1)
        a = self.agent()
        self.assertEqual(len(a.rpc("tools/list")["result"]["tools"]), 24)
        self.assertEqual(a.tool("sericon_sessions")["sessions"][0]["id"], s["id"])
        history = a.tool("sericon_read", session=s["id"])
        self.assertTrue(any("Earlier boot context" in e.get("text", "") for e in history["events"]))
        reply = a.tool("sericon_send", session=s["id"], text="help")
        self.assertEqual(reply["written"], 5)
        self.input_bytes(b"help\r")
        self.assertTrue(any(e["actor"] == "fixture-agent" for e in self.read(s["id"])["events"]))
        self.assertIsNone(self.status(s["id"])["writer"])
        self.assertEqual(a.rpc("unknown")["error"]["code"], -32601)

    def test_partial_input_blocks_other_writers_and_baud_changes(self):
        s = self.start("--no-log")
        a, b = self.agent(), self.agent()
        a.tool("sericon_send", session=s["id"], text="partial", enter=False)
        self.input_bytes(b"partial")
        self.assertIn("busy", b.tool("sericon_send", session=s["id"], text="other")["error"])
        self.assertIn("release input", b.tool("sericon_baud", session=s["id"], rate=9600)["error"])
        self.assertFalse(select.select([self.master], [], [], 0.05)[0])
        a.tool("sericon_send", session=s["id"], text=" done")
        self.input_bytes(b" done\r")
        self.assertEqual(b.tool("sericon_send", session=s["id"], text="next")["written"], 5)
        self.input_bytes(b"next\r")

    def test_terminal_raw_keys_detach_reattach_and_restore(self):
        p, terminal, slave, original = self.terminal("--port", self.port, "--no-log", "--baud", "115200")
        self.wait_terminal(terminal, b"Ctrl-]")
        s = json.loads(self.cmd("sessions", "--json").stdout)[0]
        self.sessions.append(s["id"])
        a = self.agent()
        os.write(terminal, b"abc")
        self.input_bytes(b"abc")
        self.assertIn("busy", a.tool("sericon_send", session=s["id"], text="help")["error"])
        os.write(terminal, b"\r")
        self.input_bytes(b"\r")
        a.tool("sericon_send", session=s["id"], text="help")
        self.input_bytes(b"help\r")
        self.wait_terminal(terminal, b"fixture-agent TX")
        os.write(terminal, b"\x03")
        self.input_bytes(b"\x03")
        os.write(terminal, b"\x1dd")
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)
        self.assertTrue(self.status(s["id"])["connected"])
        p2, terminal2, slave2, original2 = self.terminal("attach", s["id"])
        self.wait_terminal(terminal2, b"Ctrl-]")
        os.write(terminal2, b"\x1dq")
        self.assertEqual(p2.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave2), original2)

    def test_exclusive_port_and_bad_log_destination(self):
        dest = self.root / "not-a-directory"
        dest.write_text("keep me")
        failed = self.cmd("--port", self.port, "--detach", "--log-dir", dest, check=False)
        self.assertNotEqual(failed.returncode, 0)
        self.assertEqual(dest.read_text(), "keep me")
        s = self.start("--no-log")
        failed = self.cmd("--port", self.port, "--detach", "--no-log", check=False)
        self.assertNotEqual(failed.returncode, 0)
        self.assertIn(s["id"], failed.stderr)

    def test_disconnect_is_recorded_and_session_removed(self):
        s = self.start("--baud", "115200")
        os.close(self.master)
        self.master = None
        socket_path = self.root / "runtime" / (s["id"] + ".sock")
        eventually(lambda: not socket_path.exists())
        events = [json.loads(line) for line in (Path(s["log_directory"]) / "events.jsonl").read_text().splitlines()]
        self.assertEqual(events[-1]["kind"], "error")

    def test_bad_configuration_is_not_silently_ignored(self):
        config = self.root / "bad.toml"
        config.write_text('[logging]\nenabeld = false\n')
        result = self.cmd("--detach", "--port", self.port, "--config", config, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("enabeld", result.stderr)

    def test_startup_flags_are_not_silently_ignored_on_existing_sessions(self):
        result = self.cmd("--no-log", "attach", "0123456789ab", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("apply when starting", result.stderr)

    def test_no_argument_startup_uses_optional_user_config(self):
        config_dir = self.root / "config/sericon"
        config_dir.mkdir(parents=True)
        (config_dir / "config.toml").write_text(f'[serial]\nport = "{self.port}"\n')
        p, terminal, slave, original = self.terminal()
        self.wait_terminal(terminal, b"Ctrl-]")
        s = json.loads(self.cmd("sessions", "--json").stdout)[0]
        self.sessions.append(s["id"])
        self.assertEqual(s["baud"], 115200)
        self.assertEqual(s["detection"], "waiting")
        self.assertEqual(Path(s["log_directory"]).parent, self.root)
        os.write(self.master, b"Linux booted successfully, console available\r\nWelcome to the device\r\n")
        eventually(lambda: self.status(s["id"])["detection"] == "locked")
        os.write(terminal, b"\x1dq")
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)

    def test_terminal_signal_restores_tty_and_preserves_partial_input(self):
        s = self.start("--no-log", "--baud", "115200")
        p, terminal, slave, original = self.terminal("attach", s["id"])
        self.wait_terminal(terminal, b"Ctrl-]")
        os.write(terminal, b"unfinished")
        self.input_bytes(b"unfinished")
        p.send_signal(signal.SIGTERM)
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)
        a = self.agent()
        self.assertIn("busy", a.tool("sericon_send", session=s["id"], text="other")["error"])
        p2, terminal2, _, _ = self.terminal("attach", s["id"])
        self.wait_terminal(terminal2, b"Ctrl-]")
        os.write(terminal2, b"\x1dt\x03")
        self.input_bytes(b"\x03")
        self.assertIsNone(self.status(s["id"])["writer"])
        os.write(terminal2, b"\x1dd")
        self.assertEqual(p2.wait(timeout=5), 0)

    def test_search_cycles_highlights_and_resumes_live_without_sending_keys(self):
        s = self.start("--baud", "115200")
        p, terminal, slave, original = self.terminal("attach", s["id"])
        self.wait_terminal(terminal, b"Ctrl-]")
        os.write(self.master, b"first quiet \x1b[3")
        self.wait_terminal(terminal, b"first quiet")
        os.write(self.master, b"2mfault\x1b[0m at boot\r\nsecond quiet fault at runtime\r\n")
        self.wait_terminal(terminal, b"at runtime")
        os.write(terminal, b"\x1dfquiet fault\r")
        output = self.wait_terminal(terminal, b"Match 1")
        self.assertIn(b"sericon  /  Search", output)
        self.assertIn(b"\x1b[1;30;43mquiet fault", output)
        self.assertIn("Enter: next · Esc: options".encode(), output)
        os.write(terminal, b"\r")
        self.wait_terminal(terminal, b"Match 2")
        os.write(terminal, b"\r")
        self.wait_terminal(terminal, b"Match 1 (wrapped)")
        self.assertFalse(select.select([self.master], [], [], 0.1)[0])
        self.assertIsNone(self.status(s["id"])["writer"])

        before = self.status(s["id"])["latest_cursor"]
        os.write(self.master, b"arrived while searching\r\n")
        eventually(lambda: self.status(s["id"])["latest_cursor"] > before)
        self.assertFalse(select.select([terminal], [], [], 0.1)[0])
        agent = self.agent()
        self.assertEqual(agent.tool("sericon_send", session=s["id"], text="help")["written"], 5)
        self.input_bytes(b"help\r")
        os.write(terminal, b"q")
        output = self.wait_terminal(terminal, b"fixture-agent TX: help")
        self.assertIn(b"/  Live", output)
        self.assertIn(b"arrived while searching", output)
        self.assertTrue(self.status(s["id"])["connected"])
        os.write(terminal, b"\x1df")
        self.wait_terminal(terminal, b"Type a literal search string")
        os.write(terminal, b"\x1db")
        self.wait_terminal(terminal, b"/  Live")
        os.write(terminal, b"version\r")
        self.input_bytes(b"version\r")
        os.write(terminal, b"\x1dq")
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)
        events = [json.loads(line) for line in (Path(s["log_directory"]) / "events.jsonl").read_text().splitlines()]
        self.assertEqual([base64.b64decode(e["data_base64"]) for e in events if e["kind"] == "tx"], [b"help\r", b"version\r"])

    def test_search_edit_no_matches_utf8_and_signal_cleanup_without_logs(self):
        s = self.start("--no-log", "--baud", "115200")
        p, terminal, slave, original = self.terminal("attach", s["id"])
        self.wait_terminal(terminal, b"Ctrl-]")
        os.write(self.master, "device café ready\r\n".encode())
        self.wait_terminal(terminal, b"ready")
        os.write(terminal, b"\x1dfnonexistent\r")
        self.wait_terminal(terminal, b"No matches")
        os.write(terminal, b"f" + "caféé".encode() + b"\x7f\r")
        output = self.wait_terminal(terminal, b"Match 1")
        self.assertIn("\x1b[1;30;43mcafé".encode(), output)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 12, 60, 0, 0))
        output = self.wait_terminal(terminal, b"\x1b[11;2H")
        self.assertIn(b"q live", output)
        self.assertIn("\x1b[1;30;43mcafé".encode(), output)
        os.write(terminal, b"fwrong\x15\xff\r")
        self.wait_terminal(terminal, b"Search text must be UTF-8")
        os.write(terminal, b"\x15ready\r")
        self.wait_terminal(terminal, b"Match 1")
        self.assertFalse(select.select([self.master], [], [], 0.1)[0])
        self.assertEqual([path.name for path in self.root.iterdir()], ["runtime"])
        p.send_signal(signal.SIGTERM)
        output = self.wait_terminal(terminal, b"detached;")
        self.assertIn(b"\x1b[?1049l", output)
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)
        self.assertTrue(self.status(s["id"])["connected"])

    def test_search_preserves_partial_writer_and_detaches_or_quits_from_prompt(self):
        s = self.start("--no-log", "--baud", "115200")
        p, terminal, slave, original = self.terminal("attach", s["id"])
        self.wait_terminal(terminal, b"Ctrl-]")
        os.write(terminal, b"partial\x1df")
        self.input_bytes(b"partial")
        self.wait_terminal(terminal, b"sericon  /  Search")
        a = self.agent()
        self.assertIn("busy", a.tool("sericon_send", session=s["id"], text="other")["error"])
        os.write(terminal, b"ignored\x1dd")
        self.wait_terminal(terminal, b"\x1b[?1049l")
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)
        self.assertFalse(select.select([self.master], [], [], 0.1)[0])
        p2, terminal2, slave2, original2 = self.terminal("attach", s["id"])
        self.wait_terminal(terminal2, b"Ctrl-]")
        os.write(terminal2, b"\x1df\x1dq")
        self.wait_terminal(terminal2, b"\x1b[?1049l")
        self.assertEqual(p2.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave2), original2)

    def test_search_retained_disk_history_and_explicit_no_log_gap(self):
        for logging in (True, False):
            with self.subTest(logging=logging):
                s = self.start("--baud", "115200", *([] if logging else ["--no-log"]))
                payload = b"oldest-search-marker\r\n" + b"ordinary captured line\r\n" * 140000 + b"latest-search-marker\r\n"
                remaining = memoryview(payload)
                while remaining:
                    written = os.write(self.master, remaining[:16384])
                    remaining = remaining[written:]
                def all_received():
                    end = self.status(s["id"])["latest_cursor"]
                    tail = self.read(s["id"], max(0, end-4))
                    return "latest-search-marker" in "".join(e.get("text", "") for e in tail["events"])
                eventually(all_received)
                self.assertEqual(self.read(s["id"])["history_gap"], not logging)
                formula = self.formula_start(s["id"], 'scan_regex("(?:oldest|latest)-search-marker", "marker", 0);')
                scan = self.formula_done(s["id"], formula)
                self.assertEqual(scan["run"]["state"], "completed", scan)
                self.assertEqual({r["value"] for r in scan["results"]},
                                 {"oldest-search-marker", "latest-search-marker"} if logging else {"latest-search-marker"})
                self.assertEqual(bool(scan["run"]["warnings"]), not logging)
                p, terminal, slave, original = self.terminal("attach", s["id"])
                self.wait_terminal(terminal, b"Ctrl-]")
                os.write(terminal, b"\x1dfoldest-search-marker\r")
                output = self.wait_terminal(terminal, b"Match 1" if logging else b"No matches")
                if logging:
                    self.assertIn(b"\x1b[1;30;43moldest-search-marker", output)
                else:
                    self.assertIn(b"Earlier history unavailable", output)
                    os.write(terminal, b"\x1dflatest-search-marker\r")
                    self.wait_terminal(terminal, b"Match 1")
                os.write(terminal, b"\x1dq")
                self.wait_terminal(terminal, b"\x1b[?1049l")
                self.assertEqual(p.wait(timeout=5), 0)
                self.assertEqual(termios.tcgetattr(slave), original)
                eventually(lambda: not (self.root / "runtime" / (s["id"] + ".sock")).exists())

    def test_regex_search_cycles_variable_matches_and_switches_back_to_literal(self):
        s = self.start("--baud", "115200")
        p, terminal, slave, original = self.terminal("attach", s["id"])
        self.wait_terminal(terminal, b"Ctrl-]")
        os.write(self.master, b"ERROR \x1b[3")
        self.wait_terminal(terminal, b"ERROR ")
        os.write(self.master, b"2m17\x1b[0m\r\nwarn code=9\r\nerror 2\r\nerr.or\r\nerrXor\r\n")
        self.wait_terminal(terminal, b"errXor")
        os.write(terminal, b"\x1df\t(?i)^(error [0-9]+|warn code=\\d+)$\r")
        output = self.wait_terminal(terminal, b"Match 1")
        self.assertIn(b"[regex]", output)
        self.assertIn(b"\x1b[1;30;43mERROR 17", output)
        self.assertNotIn(b"\x1b[1;30;43m(?i)", output)
        os.write(terminal, b"\r")
        self.assertIn(b"\x1b[1;30;43mwarn code=9", self.wait_terminal(terminal, b"Match 2"))
        os.write(terminal, b"\r")
        self.assertIn(b"\x1b[1;30;43merror 2", self.wait_terminal(terminal, b"Match 3"))
        os.write(terminal, b"\r")
        self.wait_terminal(terminal, b"Match 1 (wrapped)")
        os.write(terminal, b"ferr.or\r")
        self.wait_terminal(terminal, b"Match 1")
        os.write(terminal, b"\r")
        self.assertIn(b"\x1b[1;30;43merrXor", self.wait_terminal(terminal, b"Match 2"))
        os.write(terminal, b"r\r")
        output = self.wait_terminal(terminal, b"Match 1")
        self.assertIn(b"[literal]", output)
        self.assertIn(b"\x1b[1;30;43merr.or", output)
        os.write(terminal, b"\r")
        self.wait_terminal(terminal, b"Match 1 (wrapped)")
        self.assertFalse(select.select([self.master], [], [], 0.1)[0])
        os.write(terminal, b"q")
        self.wait_terminal(terminal, b"/  Live")
        self.assertTrue(self.status(s["id"])["connected"])
        os.write(terminal, b"\x1dq")
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)

    def test_regex_invalid_pattern_can_be_edited_and_zero_width_matches_advance(self):
        s = self.start("--no-log", "--baud", "115200")
        p, terminal, slave, original = self.terminal("attach", s["id"])
        self.wait_terminal(terminal, b"Ctrl-]")
        os.write(self.master, "café\r\nsecond line\r\n".encode())
        self.wait_terminal(terminal, b"second line")
        os.write(terminal, b"\x1df\t[\r")
        output = self.wait_terminal(terminal, b"Invalid regex")
        self.assertIn(b"unclosed character class", output)
        os.write(terminal, b"\x15^caf.+$\r")
        output = self.wait_terminal(terminal, b"Match 1")
        self.assertIn("\x1b[1;30;43mcafé".encode(), output)
        os.write(terminal, b"f^\r")
        output = self.wait_terminal(terminal, b"Match 1 (zero-width)")
        self.assertIn("\x1b[1;30;43m▏".encode(), output)
        os.write(terminal, b"\r")
        self.wait_terminal(terminal, b"Match 2 (zero-width)")
        os.write(terminal, b"\r")
        self.wait_terminal(terminal, b"Match 1 (wrapped) (zero-width)")
        self.assertFalse(select.select([self.master], [], [], 0.1)[0])
        self.assertEqual([path.name for path in self.root.iterdir()], ["runtime"])
        os.write(terminal, b"q\x1dq")
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)

    def test_search_help_preserves_prompt_results_and_uart_input(self):
        s = self.start("--no-log", "--baud", "115200")
        p, terminal, slave, original = self.terminal("attach", s["id"])
        self.wait_terminal(terminal, b"Ctrl-]")
        os.write(terminal, b"\x1d?")
        output = self.wait_terminal(terminal, b"Enter/q: close help")
        self.assertIn(b"Live terminal", output)
        pages = bytearray()
        total_lines = int(re.search(rb"Lines [0-9]+.*? of ([0-9]+)", output).group(1))
        for _ in range((total_lines + 9) // 10):  # Space advances ten lines; help can grow.
            os.write(terminal, b" ")
            time.sleep(.12)
            while select.select([terminal], [], [], .01)[0]:
                pages.extend(os.read(terminal, 65536))
            if b"Regex examples:" in pages:
                break
        self.assertIn(b"Regex examples:", pages)
        os.write(terminal, b"q")
        self.wait_terminal(terminal, b"/  Live")
        os.write(self.master, b"ERROR 17\r\nERROR 29\r\n")
        self.wait_terminal(terminal, b"ERROR 29")
        os.write(terminal, b"\x1df\tERROR \\d+\x1d?")
        output = self.wait_terminal(terminal, b"Enter/q: close help")
        self.assertIn(b"Search help", output)
        self.assertIn(b"Regex examples:", output)
        os.write(terminal, b"q\r")
        output = self.wait_terminal(terminal, b"Match 1")
        self.assertIn(b"[regex]", output)
        self.assertIn(b"\x1b[1;30;43mERROR 17", output)
        os.write(terminal, b"\x1d?")
        self.wait_terminal(terminal, b"Enter/q: close help")
        os.write(terminal, b"\r")
        self.wait_terminal(terminal, b"Match 1")
        os.write(terminal, b"\r")
        self.wait_terminal(terminal, b"Match 2")
        self.assertFalse(select.select([self.master], [], [], 0.1)[0])
        self.assertIsNone(self.status(s["id"])["writer"])
        os.write(terminal, b"\x1d?\x1db")
        self.wait_terminal(terminal, b"/  Live")
        self.assertTrue(self.status(s["id"])["connected"])
        os.write(terminal, b"\x1dq")
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)


    def formula_start(self, session, source, interactive=False, params=None, parameters=None, **options):
        request = {"op": "formula_start", "formula": {"name": "fixture", "source": source,
                   "interactive": interactive, "parameters": parameters or {}},
                   "params": params or {}, "initiator": "fixture", **options}
        result = self.control(session, request)
        self.assertIn("result", result, result)
        return result["result"]["id"]

    def formula_read(self, session, run, after=0, limit=32):
        result = self.control(session, {"op": "formula_read", "run": run, "after": after, "limit": limit})
        self.assertIn("result", result, result)
        return result["result"]

    def formula_done(self, session, run, timeout=5):
        return eventually(lambda: (v if (v := self.formula_read(session, run))["run"]["state"] != "running" else None), timeout=timeout)

    def test_formula_builtins_split_lines_deduplicate_and_do_not_transmit(self):
        s = self.start("--baud", "115200", "--no-log")
        os.write(self.master, b'password="secret phrase"\r\npassword:\r\npasswd=\x1b[32mhunt')
        time.sleep(0.04)
        os.write(self.master, b'er2\x1b[0m\r\npasswd=hunter2\r\npassword=********\r\n'
                 b'https://api.example.test:8443/v1?q=ok\r\n192.168.1.2 999.1.1.1 2001:db8::1\r\n'
                 b'https://api.example.test:8443/v1?q=ok\r\nhttp://[::1]\r\n::ffff:192.0.2.1\r\n')
        eventually(lambda: any("api.example" in e.get("text", "") for e in self.read(s["id"])["events"]))
        passwords = json.loads(self.cmd("formulas", "run", "passwords", "--session", s["id"]).stdout)
        result = self.formula_done(s["id"], passwords["id"])
        self.assertEqual(result["run"]["state"], "completed", result)
        values = {r["value"]: r for r in result["results"]}
        self.assertEqual(set(values), {"secret phrase", "hunter2"})
        self.assertEqual(values["hunter2"]["count"], 2)
        self.assertIn("cursor", values["hunter2"]["locations"][0])
        endpoints = json.loads(self.cmd("formulas", "run", "endpoints", "--session", s["id"]).stdout)
        result = self.formula_done(s["id"], endpoints["id"])
        self.assertEqual(result["run"]["state"], "completed", result)
        values = {r["value"]: r for r in result["results"]}
        self.assertIn("192.168.1.2", values)
        self.assertIn("2001:db8::1", values)
        self.assertIn("::ffff:192.0.2.1", values)
        self.assertIn("http://[::1]", values)
        self.assertNotIn("999.1.1.1", values)
        self.assertEqual(values["https://api.example.test:8443/v1?q=ok"]["count"], 2)
        self.assertFalse(select.select([self.master], [], [], 0.05)[0])
        self.assertIsNone(self.status(s["id"])["writer"])
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), ["runtime"])

    def test_formula_ai_create_validate_run_disconnect_and_paginate(self):
        s = self.start("--baud", "115200")
        agent = self.agent()
        definition = {"name": "agent-script", "source": 'sleep_ms(150); for i in 0..params.count { emit(#{value:i}); }',
                      "parameters": {"count": {"type": "integer", "required": True}}, "description": "Fixture formula"}
        for builtin in ["linux-probe", "linux-list", "linux-download"]:
            reply = agent.tool("sericon_formula_put", formula=dict(definition, name=builtin), replace=True)
            self.assertIn("built-in formulas cannot be replaced", reply["error"])
        self.assertTrue(agent.tool("sericon_formula_validate", formula=definition, params={"count": 3})["valid"])
        self.assertIn("error", agent.tool("sericon_formula_validate", formula=definition, params={"count": "bad"}))
        saved = agent.tool("sericon_formula_put", formula=definition)
        self.assertIn("path", saved, saved)
        self.assertEqual(Path(saved["path"]).stat().st_mode & 0o777, 0o600)
        self.assertIn("error", agent.tool("sericon_formula_put", formula=definition))
        broken = dict(definition, source="let broken = ;")
        self.assertIn("error", agent.tool("sericon_formula_put", formula=broken, replace=True))
        self.assertEqual(agent.tool("sericon_formula_show", name="agent-script")["formula"]["source"], definition["source"])
        run = agent.tool("sericon_formula_run", session=s["id"], name="agent-script", params={"count": 3})
        self.assertIn("id", run, run)
        agent.close()
        self.agents.remove(agent)
        result = self.formula_done(s["id"], run["id"])
        self.assertEqual(result["run"]["state"], "completed", result)
        self.assertEqual(result["run"]["initiator"], "fixture-agent")
        first = self.formula_read(s["id"], run["id"], limit=2)
        self.assertTrue(first["has_more"])
        last = self.formula_read(s["id"], run["id"], after=first["next_cursor"])
        self.assertEqual([r["value"] for r in first["results"] + last["results"]], [0, 1, 2])
        archive = Path(result["run"]["archive"])
        eventually(lambda: json.loads(archive.read_text())["run"]["state"] == "completed")
        record = json.loads(archive.read_text())
        self.assertEqual(record["definition"]["source"], definition["source"])
        self.assertEqual(record["parameters"], {"count": 3})
        self.assertEqual(archive.stat().st_mode & 0o777, 0o600)

    def test_formula_expect_fast_split_rx_preserves_binary_and_ignores_tx(self):
        s = self.start("--baud", "115200", "--no-log")
        source = '''mark(); send_line("PROMPT");
let early = expect_text("PROMPT", 50); emit(#{early:early.matched});
let reply = expect_text("READY>", 2000);
if !reply.matched { throw "missing prompt"; }
emit(#{hex:hex_encode(reply.before),matched:reply.matched});
let suffix = read_bytes(32, 1000); emit(#{suffix:hex_encode(suffix)});'''
        run = self.formula_start(s["id"], source, interactive=True)
        self.input_bytes(b"PROMPT\r")
        eventually(lambda: self.formula_read(s["id"], run)["results"])
        os.write(self.master, b"\x00\xffREA")
        time.sleep(0.03)
        os.write(self.master, b"DY>\x01\xfe")
        result = self.formula_done(s["id"], run)
        self.assertEqual(result["run"]["state"], "completed", result)
        self.assertEqual(result["results"], [{"early": False}, {"hex": "00ff", "matched": True}, {"suffix": "01fe"}])
        self.assertIsNone(self.status(s["id"])["writer"])
        tx = [e for e in self.read(s["id"])["events"] if e["kind"] == "tx"]
        self.assertEqual(tx[0]["actor"], "formula:" + run)

    def test_formula_memory_dump_fixture_streams_checked_chunks_to_private_artifact(self):
        s = self.start("--baud", "115200", "--no-log")
        out = self.root / "output"
        source = '''artifact_open(params.filename);
let total = 0;
for i in 0..2 {
    mark(); send_line("chunk " + i);
    let reply = expect_text("READY>", 2000);
    if !reply.matched { throw "missing response"; }
    let rows = matches("DATA ([0-9a-f]+)", reply.text);
    if rows.len != 1 { throw "expected one data row"; }
    let block = hex_decode(rows[0].groups[1]);
    if block.len != 4 { throw "short block"; }
    total += artifact_write(params.filename, block);
    progress(total, 8, "Reading fixture memory");
}
artifact_close(params.filename); emit(#{bytes:total});'''
        run = self.formula_start(s["id"], source, interactive=True,
                                 parameters={"filename": {"type": "string", "required": True}},
                                 params={"filename": "dump.bin"}, output_dir=str(out))
        self.input_bytes(b"chunk 0\r")
        os.write(self.master, b"DATA 00ff0102\r\nREADY>")
        self.input_bytes(b"chunk 1\r")
        os.write(self.master, b"DATA 03040506\r\nREADY>")
        result = self.formula_done(s["id"], run)
        self.assertEqual(result["run"]["state"], "completed", result)
        artifact = result["run"]["artifacts"][0]
        payload = bytes.fromhex("00ff010203040506")
        self.assertEqual(Path(artifact["path"]).read_bytes(), payload)
        self.assertEqual(artifact["sha256"], hashlib.sha256(payload).hexdigest())
        self.assertTrue(artifact["complete"])
        self.assertEqual(artifact["bytes"], 8)
        self.assertEqual(Path(artifact["path"]).stat().st_mode & 0o777, 0o600)
        self.assertEqual(Path(artifact["path"]).parent.stat().st_mode & 0o777, 0o700)
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), ["output", "runtime"])

    def test_formula_cancel_and_takeover_revoke_future_writes(self):
        s = self.start("--baud", "115200", "--no-log")
        run = self.formula_start(s["id"], 'send("partial"); sleep_ms(5000); send_line("must-not-send");', interactive=True)
        self.input_bytes(b"partial")
        self.control(s["id"], {"op": "formula_cancel", "run": run})
        result = self.formula_done(s["id"], run)
        self.assertEqual(result["run"]["state"], "cancelled", result)
        self.assertTrue(result["run"]["writer_retained"])
        self.assertFalse(select.select([self.master], [], [], 0.05)[0])
        # Take over and release; the old formula must remain unable to reacquire.
        self.control(s["id"], {"op": "claim", "actor": "human", "client_id": "fixture-human", "takeover": True})
        self.control(s["id"], {"op": "release", "client_id": "fixture-human"})
        result = self.control(s["id"], {"op": "send", "actor": "formula:" + run, "client_id": "formula-" + run,
                              "data_base64": base64.b64encode(b"bad\r").decode()})
        self.assertIn("error", result)
        second = self.formula_start(s["id"], 'send_line("start"); sleep_ms(5000); send_line("bad");', interactive=True)
        self.input_bytes(b"start\r")
        self.control(s["id"], {"op": "claim", "actor": "human", "client_id": "fixture-human", "takeover": True})
        self.control(s["id"], {"op": "release", "client_id": "fixture-human"})
        result = self.formula_done(s["id"], second)
        self.assertEqual(result["run"]["state"], "cancelled", result)
        self.assertIn("takeover", result["run"]["error"])
        self.assertFalse(select.select([self.master], [], [], 0.05)[0])

    def test_formula_busy_readonly_deadline_and_partial_artifact_failure(self):
        s = self.start("--baud", "115200", "--no-log")
        self.control(s["id"], {"op": "claim", "actor": "human", "client_id": "fixture-human"})
        run = self.formula_start(s["id"], 'send_line("bad");', interactive=True)
        self.assertIn("busy", self.formula_done(s["id"], run)["run"]["error"])
        self.control(s["id"], {"op": "release", "client_id": "fixture-human"})
        run = self.formula_start(s["id"], 'send_line("bad");')
        self.assertEqual(self.formula_done(s["id"], run)["run"]["state"], "failed")
        run = self.formula_start(s["id"], 'loop { let x = 1 + 1; }', timeout_ms=100)
        self.assertEqual(self.formula_done(s["id"], run)["run"]["state"], "failed")
        run = self.formula_start(s["id"], 'artifact_open("partial.bin"); artifact_write("partial.bin", hex_decode("00ff")); throw "short read";', output_dir=str(self.root / "out"))
        result = self.formula_done(s["id"], run)
        self.assertEqual(result["run"]["state"], "failed", result)
        self.assertFalse(result["run"]["artifacts"][0]["complete"])
        self.assertEqual(result["run"]["artifacts"][0]["bytes"], 2)
        self.assertFalse(select.select([self.master], [], [], 0.05)[0])

    def test_formula_config_relative_script_and_source_revision_snapshot(self):
        config_dir = self.root / "custom"
        config_dir.mkdir()
        script = config_dir / "custom.rhai"
        script.write_text('sleep_ms(200); emit(#{value:params.value});')
        config = config_dir / "config.toml"
        config.write_text('[[formulas]]\nname="custom"\nscript="custom.rhai"\n[formulas.parameters.value]\ntype="integer"\ndefault=17\n')
        s = self.start("--baud", "115200", "--no-log")
        run = json.loads(self.cmd("--config", config, "formulas", "run", "custom", "--session", s["id"]).stdout)
        script.write_text('emit(#{value:99});')
        result = self.formula_done(s["id"], run["id"])
        self.assertEqual(result["results"], [{"value": 17}])
        changed = json.loads(self.cmd("--config", config, "formulas", "show", "custom").stdout)
        self.assertNotEqual(changed["revision"], run["revision"])

    def test_formula_terminal_picker_runs_context_and_restores_live(self):
        s = self.start("--baud", "115200", "--no-log")
        p, terminal, slave, original = self.terminal("attach", s["id"])
        self.wait_terminal(terminal, b"Ctrl-]")
        os.write(self.master, b"endpoint https://fixture.test/api\r\n")
        self.wait_terminal(terminal, b"fixture.test")
        os.write(terminal, b"\x1dx")
        self.wait_terminal(terminal, b"sericon  /  Formulas")
        os.write(terminal, b"\r")
        self.wait_terminal(terminal, b"Run formula")
        os.write(terminal, b"\r")
        output = self.wait_terminal(terminal, b"Completed")
        self.assertIn(b"https://fixture.test/api", output)
        os.write(terminal, b"\r")
        output = self.wait_terminal(terminal, b"Finding context")
        self.assertIn(b"context", output)
        os.write(self.master, b"capture continued\r\n")
        os.write(terminal, b"\x1db")
        self.wait_terminal(terminal, b"capture continued")
        self.assertFalse(select.select([self.master], [], [], 0.05)[0])
        os.write(terminal, b"\x1dq")
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)

    def test_live_header_and_notices_do_not_split_uart_lines(self):
        s = self.start()
        p, terminal, slave, original = self.terminal("attach", s["id"])
        self.wait_terminal(terminal, b"Ctrl-]")
        payload = b"2291 [3666092] E (3668044) nvs_read_"
        os.write(self.master, payload)
        self.wait_terminal(terminal, b"nvs_read_")
        eventually(lambda: self.status(s["id"])["detection"] == "locked")
        self.cmd("baud", s["id"], "115200")
        self.wait_terminal(terminal, b"fixed")
        os.write(self.master, b"blob: NVS get blob error\r\n")
        output = self.wait_terminal(terminal, b"nvs_read_blob: NVS get blob error")
        self.assertIn(b"2291 [3666092] E (3668044)", output)
        received = b"".join(base64.b64decode(e["data_base64"]) for e in self.read(s["id"])["events"] if e["kind"] == "rx")
        self.assertEqual(received, payload + b"blob: NVS get blob error\r\n")
        self.assertFalse(select.select([self.master], [], [], 0.05)[0])
        os.write(terminal, b"\x1d?\x1dd")
        self.wait_terminal(terminal, b"\x1b[?1049l")
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)
        self.assertTrue(self.status(s["id"])["connected"])

    def test_menu_opens_views_and_restores_queries_and_formula_options(self):
        s = self.start("--baud", "115200", "--no-log")
        p, terminal, slave, original = self.terminal("attach", s["id"])
        self.wait_terminal(terminal, b"Ctrl-]")
        os.write(self.master, b"boot ready\r\n")
        self.wait_terminal(terminal, b"boot ready")
        os.write(terminal, b"\x1dm")
        output = self.wait_terminal(terminal, b"/  Menu")
        self.assertIn("› Live terminal".encode(), output)
        # Fragmented arrow sequence must be consumed before Enter opens search.
        os.write(terminal, b"\x1b")
        time.sleep(0.03)
        os.write(terminal, b"[B\rboot")
        self.wait_terminal(terminal, b"[literal] /boot")
        os.write(terminal, b"\x1dm")
        self.wait_terminal(terminal, b"/  Menu")
        os.write(terminal, b"q")
        self.wait_terminal(terminal, b"[literal] /boot")
        os.write(terminal, b"\r")
        self.wait_terminal(terminal, b"Match 1")
        os.write(terminal, b"\x1dm")
        self.wait_terminal(terminal, b"/  Menu")
        os.write(terminal, b"\x1b")
        self.wait_terminal(terminal, b"Match 1")
        # SS3 and CSI arrows both work; choosing Formulas replaces search.
        os.write(terminal, b"\x1dm\x1bOB\x1b[B\r")
        self.wait_terminal(terminal, b"/  Formulas")
        os.write(terminal, b"\r")
        self.wait_terminal(terminal, b"Run formula")
        os.write(terminal, b"\x1b[B" * 3 + b"\r")
        self.wait_terminal(terminal, b"Options JSON")
        options = b'{"params":{},"output_dir":"./fixture-output"}'
        os.write(terminal, b"\x15" + options)
        self.wait_terminal(terminal, b"fixture-output")
        os.write(terminal, b"\x1dm")
        self.wait_terminal(terminal, b"/  Menu")
        os.write(terminal, b"q")
        output = self.wait_terminal(terminal, b"Options JSON")
        self.assertIn(options, output)
        os.write(terminal, b"\x1dm" + b"\x1b[B" * 3 + b"\r")
        self.wait_terminal(terminal, b"/  Help")
        os.write(terminal, b"\x1dm")
        self.wait_terminal(terminal, b"/  Menu")
        os.write(terminal, b"q")
        self.wait_terminal(terminal, b"/  Help")
        os.write(terminal, b"q")
        self.wait_terminal(terminal, b"/  Live")
        self.assertFalse(select.select([self.master], [], [], 0.05)[0])
        self.assertIsNone(self.status(s["id"])["writer"])
        os.write(terminal, b"\x1dq")
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)

    def test_menu_capture_partial_writer_and_detach_then_quit(self):
        s = self.start("--baud", "115200", "--no-log")
        p, terminal, slave, original = self.terminal("attach", s["id"])
        self.wait_terminal(terminal, b"Ctrl-]")
        os.write(terminal, b"unfinished")
        self.input_bytes(b"unfinished")
        writer = self.status(s["id"])["writer"]
        os.write(terminal, b"\x1dm")
        self.wait_terminal(terminal, b"/  Menu")
        before = self.status(s["id"])["latest_cursor"]
        payload = b"capture behind popup\r\n"
        os.write(self.master, payload)
        eventually(lambda: self.status(s["id"])["latest_cursor"] > before)
        agent = self.agent()
        page = agent.tool("sericon_read", session=s["id"], after=before)
        self.assertEqual(b"".join(base64.b64decode(e["data_base64"]) for e in page["events"] if e["kind"] == "rx"), payload)
        self.assertIn("busy", agent.tool("sericon_send", session=s["id"], text="other")["error"])
        self.assertEqual(self.status(s["id"])["writer"], writer)
        os.write(terminal, b"q")
        self.wait_terminal(terminal, b"capture behind popup")
        os.write(terminal, b"\x1dm" + b"\x1b[B" * 9)
        output = self.wait_terminal(terminal, "› Detach".encode())
        self.assertIn(b"keep capture running", output)
        os.write(terminal, b"\r")
        self.wait_terminal(terminal, b"\x1b[?1049l")
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)
        self.assertTrue(self.status(s["id"])["connected"])
        self.assertEqual(self.status(s["id"])["writer"], writer)
        self.assertFalse(select.select([self.master], [], [], 0.05)[0])
        p, terminal, slave, original = self.terminal("attach", s["id"])
        self.wait_terminal(terminal, b"Ctrl-]")
        os.write(terminal, b"\x1dm" + b"\x1b[B" * 10)
        output = self.wait_terminal(terminal, "› Quit / stop session".encode())
        self.assertIn(b"Stop capture and close the UART", output)
        os.write(terminal, b"\r")
        self.wait_terminal(terminal, b"\x1b[?1049l")
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)
        eventually(lambda: not (self.root / "runtime" / (s["id"] + ".sock")).exists())
        self.assertFalse(select.select([self.master], [], [], 0.05)[0])

    def test_menu_resize_no_color_and_signal_restore(self):
        self.env["NO_COLOR"] = "1"
        s = self.start("--baud", "115200", "--no-log")
        p, terminal, slave, original = self.terminal("attach", s["id"])
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 28, 92, 0, 0))
        self.wait_terminal(terminal, b"Ctrl-]")
        os.write(terminal, b"\x1dm")
        self.wait_terminal(terminal, b"/  Menu")
        # Even when centering keeps the same coordinates, the repainted
        # background must not erase the popup.
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 28, 91, 0, 0))
        self.wait_terminal(terminal, b"/  Menu")
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 12, 40, 0, 0))
        output = self.wait_terminal(terminal, b"Esc/q back")
        self.assertNotRegex(output, rb"\x1b\[[0-9;]*(?:3[0-7]|4[0-7]|90)m")
        os.write(terminal, b"\x1b[F")
        output = self.wait_terminal(terminal, "› Quit / stop session".encode())
        self.assertIn(b"\x1b[7m", output)
        for row, col in re.findall(rb"\x1b\[(\d+);(\d+)H", output):
            self.assertLessEqual(int(row), 12)
            self.assertLessEqual(int(col), 40)
        os.write(terminal, b"\x1b[H\x1b[B")
        self.wait_terminal(terminal, "› Search history".encode())
        os.write(terminal, b"\x1b")
        self.wait_terminal(terminal, b"sericon / Live")
        os.write(terminal, b"\x1dm")
        self.wait_terminal(terminal, b"/  Menu")
        p.send_signal(signal.SIGTERM)
        self.wait_terminal(terminal, b"\x1b[?1049l")
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)
        self.assertTrue(self.status(s["id"])["connected"])
        self.assertFalse(select.select([self.master], [], [], 0.05)[0])
        self.assertIsNone(self.status(s["id"])["writer"])
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), ["runtime"])

    def test_tui_empty_runs_progress_and_errors_are_readable(self):
        s = self.start("--baud", "115200", "--no-log")
        p, terminal, slave, original = self.terminal("attach", s["id"])
        self.wait_terminal(terminal, b"Ctrl-]")
        os.write(terminal, b"\x1dxr")
        self.wait_terminal(terminal, b"No formula runs yet.")
        os.write(terminal, b"f\r\r")
        output = self.wait_terminal(terminal, b"No findings in this run.")
        self.assertIn("Completed · 0 findings".encode(), output)
        self.assertNotIn(b'"done":', output)
        os.write(terminal, b"v")
        output = self.wait_terminal(terminal, b"Run details")
        self.assertIn(b"Status: completed", output)
        os.write(terminal, b"q")
        self.wait_terminal(terminal, b"No findings in this run.")
        run = self.formula_start(s["id"], 'progress(3, 10, "Reading blocks"); sleep_ms(1500); throw "Target prompt missing";')
        os.write(terminal, b"rj\r")
        output = self.wait_terminal(terminal, b"Reading blocks")
        self.assertIn(b"30% (3/10)", output)
        output = self.wait_terminal(terminal, b"Formula failed.")
        self.assertIn(b"Target prompt missing", output)
        self.assertEqual(self.formula_read(s["id"], run)["run"]["state"], "failed")
        os.write(self.master, b"still capturing during formula screens\r\n")
        os.write(terminal, b"\x1db")
        self.wait_terminal(terminal, b"still capturing")
        self.assertFalse(select.select([self.master], [], [], 0.05)[0])
        os.write(terminal, b"\x1dq")
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)

    def test_tui_no_color_small_windows_help_and_unicode_input(self):
        self.env["NO_COLOR"] = "1"
        s = self.start("--baud", "115200", "--no-log")
        p, terminal, slave, original = self.terminal("attach", s["id"])
        self.wait_terminal(terminal, b"Ctrl-]")
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 12, 40, 0, 0))
        os.write(terminal, b"\x1dx\r")
        output = self.wait_terminal(terminal, b"Enter choose")
        self.assertNotRegex(output, rb"\x1b\[[0-9;]*(?:3[0-7]|4[0-7]|90)m")
        os.write(terminal, b"\x1d?")
        self.wait_terminal(terminal, b"Formulas help")
        os.write(terminal, b"q")
        output = self.wait_terminal(terminal, b"Run formula")
        self.assertIn(b"Analyse retained history", output)
        os.write(terminal, b"\x1db\x1df" + ("界" * 30 + "tail").encode())
        output = self.wait_terminal(terminal, b"tail")
        self.assertIn("…".encode(), output)
        for row, col in re.findall(rb"\x1b\[(\d+);(\d+)H", output):
            self.assertLessEqual(int(row), 12)
            self.assertLessEqual(int(col), 40)
        os.write(terminal, b"\x15ready\r")
        self.wait_terminal(terminal, b"No matches")
        os.write(terminal, b"q\x1d?")
        self.wait_terminal(terminal, b"q close")
        os.write(self.master, b"ready from device\r\n")
        os.write(terminal, b" ")
        self.wait_terminal(terminal, b"Lines ")
        os.write(terminal, b"q")
        self.wait_terminal(terminal, b"ready from device")
        self.assertFalse(select.select([self.master], [], [], 0.05)[0])
        self.assertIsNone(self.status(s["id"])["writer"])
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), ["runtime"])
        os.write(terminal, b"\x1dq")
        self.assertEqual(p.wait(timeout=5), 0)
        self.assertEqual(termios.tcgetattr(slave), original)

    def test_formula_boot_interrupt_fixture_raw_bytes_and_explicit_release(self):
        s = self.start("--baud", "115200", "--no-log")
        source = '''emit(#{armed:true});
let boot = expect_text("stop autoboot:", 2000);
if !boot.matched { throw "boot message missing"; }
send_bytes(hex_decode("20"));
let prompt = expect_bytes(hex_decode("3d3e"), 2000);
if !prompt.matched { throw "prompt missing"; }
release(); emit(#{interrupted:true});'''
        run = self.formula_start(s["id"], source, interactive=True)
        eventually(lambda: self.formula_read(s["id"], run)["results"])
        os.write(self.master, b"Hit any key to stop auto")
        time.sleep(0.03)
        os.write(self.master, b"boot: 2\r\n")
        self.input_bytes(b" ")
        os.write(self.master, b"=>")
        result = self.formula_done(s["id"], run)
        self.assertEqual(result["run"]["state"], "completed", result)
        self.assertEqual(result["results"][-1], {"interrupted": True})
        self.assertIsNone(self.status(s["id"])["writer"])

    def test_tplink_recipe_waits_for_fresh_boot_cleans_prompt_and_releases(self):
        s = self.start("--baud", "115200", "--no-log")
        os.write(self.master, b"Old U-Boot banner\r\nMT7628 # ")
        eventually(lambda: any("MT7628" in e.get("text", "") for e in self.read(s["id"])["events"]))
        source = (Path(__file__).resolve().parents[1] / "formulas/examples/tplink-uboot-interrupt.rhai").read_text()
        run = self.formula_start(s["id"], source, interactive=True)
        eventually(lambda: self.formula_read(s["id"], run)["results"])
        self.assertFalse(select.select([self.master], [], [], 0.1)[0])
        os.write(self.master, b"Fresh U-Bo")
        self.assertFalse(select.select([self.master], [], [], 0.05)[0])
        os.write(self.master, b"ot 1.1.3\r\n")
        self.input_bytes(b"tpl" * 256)
        os.write(self.master, b"System Enter Boot Command Line Interface.\r\nMT7628 # tpl")
        self.input_bytes(b"\x15\r")
        # Completion requires a fresh prompt AFTER clearing the trailing input.
        self.assertEqual(self.formula_read(s["id"], run)["run"]["state"], "running")
        os.write(self.master, b"\r\nMT7628 # ")
        result = self.formula_done(s["id"], run)
        self.assertEqual(result["run"]["state"], "completed", result)
        self.assertEqual(result["results"][-1]["stage"], "interrupted")
        self.assertFalse(result["run"]["writer_retained"])
        self.assertIsNone(self.status(s["id"])["writer"])
        self.assertFalse(select.select([self.master], [], [], 0.1)[0])

    def test_tplink_recipe_stops_transmitting_when_kernel_boots(self):
        s = self.start("--baud", "115200", "--no-log")
        source = (Path(__file__).resolve().parents[1] / "formulas/examples/tplink-uboot-interrupt.rhai").read_text()
        run = self.formula_start(s["id"], source, interactive=True)
        eventually(lambda: self.formula_read(s["id"], run)["results"])
        os.write(self.master, b"U-Boot 1.1.3\r\n")
        self.input_bytes(b"tpl" * 256)
        os.write(self.master, b"## Booting image at bc010000 ...\r\n")
        result = self.formula_done(s["id"], run)
        self.assertEqual(result["run"]["state"], "failed", result)
        self.assertIn("interrupt window was missed", result["run"]["error"])
        self.assertTrue(result["run"]["writer_retained"])
        self.assertFalse(select.select([self.master], [], [], 0.1)[0])

    def test_formula_limits_are_errors_and_stop_archives_cancellation(self):
        s = self.start("--baud", "115200")
        run = self.formula_start(s["id"], 'for i in 0..5001 { emit(#{n:i}); }')
        result = self.formula_done(s["id"], run)
        self.assertEqual(result["run"]["state"], "failed", result)
        self.assertEqual(result["run"]["result_count"], 5000)
        run = self.formula_start(s["id"], 'let text = "a"; for i in 0..16 { text += text; } print(text);')
        result = self.formula_done(s["id"], run)
        self.assertEqual(result["run"]["state"], "failed", result)
        run = self.formula_start(s["id"], 'artifact_open("../bad");', output_dir=str(self.root / "out"))
        self.assertEqual(self.formula_done(s["id"], run)["run"]["state"], "failed")
        self.assertFalse((self.root / "out").exists())
        run = self.formula_start(s["id"], 'emit(#{started:true}); sleep_ms(5000);')
        eventually(lambda: self.formula_read(s["id"], run)["results"])
        archive = Path(self.formula_read(s["id"], run)["run"]["archive"])
        self.cmd("stop", s["id"])
        eventually(lambda: not (self.root / "runtime" / (s["id"] + ".sock")).exists())
        self.assertEqual(json.loads(archive.read_text())["run"]["state"], "cancelled")


    def files_start(self, session, path, name="dump.bin", **options):
        return self.formula_start(session, 'emit(linux_download(params.path, params.name));',
                                  interactive=True, params={"path":str(path), "name":name},
                                  parameters={"path":{"type":"string"},"name":{"type":"string"}},
                                  output_dir=str(self.root / "downloads"), **options)

    def test_helper_install_readback_noise_and_mcp_download(self):
        s = self.start("--baud", "115200", "--no-log")
        inventory = json.loads(self.cmd("files", "helpers").stdout)
        if not any(p["arch"] == "x86_64" for p in inventory):
            self.skipTest("build helper payloads with python3 helper/build.py")
        hits = []
        def corrupt(command, output):
            if ("/p0001'" in command or ' read ' in command) and len(hits)<2:
                hits.append(command)
                return corrupt_chunk(output,"log")
            return output
        dut = self.root / 'binary'; content=bytes(range(256))*11+b'\x00\xff\r\n'
        dut.write_bytes(content)
        agent = self.agent()
        with Shell(self.master,corrupt=corrupt) as shell:
            run=agent.tool("sericon_files_helper_install",session=s['id'],arch='x86_64',path=str(self.root))['id']
            result=self.formula_done(s['id'],run,timeout=30)
            self.assertEqual(result['run']['state'],'completed',result)
            installed=result['results'][-1]
            helper=Path(installed['helper'])
            self.assertTrue(installed['verified_before_execution'])
            self.assertEqual(hashlib.sha256(helper.read_bytes()).hexdigest(),installed['sha256'])
            self.assertEqual(helper.stat().st_mode & 0o777,0o700)
            self.assertEqual(list(helper.parent.iterdir()),[helper])
            run=agent.tool('sericon_files_list',session=s['id'],helper=str(helper),path=str(self.root))['id']
            listing=self.formula_done(s['id'],run)
            self.assertEqual(listing['run']['state'],'completed',listing)
            self.assertTrue(any(e.get('path')==str(dut) for e in listing['results']))
            run=agent.tool('sericon_files_download',session=s['id'],helper=str(helper),path=str(dut),output_dir=str(self.root/'out'))['id']
            result=self.formula_done(s['id'],run)
        self.assertEqual(result['run']['state'],'completed',result)
        self.assertEqual(Path(result['run']['artifacts'][0]['path']).read_bytes(),content)
        self.assertIsNone(self.status(s['id'])['writer'])
        self.assertEqual(len(hits),2)
        self.assertTrue(any('chmod 700' in c for c in shell.commands))

    def test_helper_corrupt_elf_never_executes(self):
        s=self.start('--baud','115200')
        if not json.loads(self.cmd('files','helpers').stdout): self.skipTest('no bundled payloads')
        def corrupt(command,output):
            if '/p* >' in command: return corrupt_chunk(output,'alphabet')
            return output
        with Shell(self.master,corrupt=corrupt) as shell:
            run=json.loads(self.cmd('files','helper-install','--arch','x86_64','--directory',self.root).stdout)['id']
            result=self.formula_done(s['id'],run,timeout=30)
        self.assertEqual(result['run']['state'],'failed',result)
        self.assertIn('helper not executed',result['run']['error'])
        self.assertFalse(any('chmod 700' in c or '"$t" info' in c for c in shell.commands))
        self.assertEqual(len([c for c in shell.commands if '/p* >' in c]),3)
        self.assertIsNone(self.status(s['id'])['writer'])

    def test_helper_unknown_arch_does_not_write_uart(self):
        s=self.start('--baud','115200')
        run=json.loads(self.cmd('files','helper-install','--arch','unknown','--directory',self.root).stdout)['id']
        result=self.formula_done(s['id'],run)
        self.assertEqual(result['run']['state'],'failed')
        self.assertIn('no bundled helper',result['run']['error'])
        self.assertFalse(any(e['kind']=='tx' for e in self.read(s['id'])['events']))

    def test_files_download_retries_midline_and_valid_alphabet_noise(self):
        s = self.start("--baud", "115200", "--no-log")
        dut = self.root / "weird ' $(touch SHOULD_NOT_EXIST) file"
        content = bytes(range(256)) * 20 + b"\r\n\x00\xfflast"
        dut.write_bytes(content)
        hits = []
        def corrupt(command, output):
            if "skip=1 count=1" in command and len(hits) < 2:
                hits.append(command)
                return corrupt_chunk(output, "alphabet" if len(hits) == 1 else "log")
            return output
        with Shell(self.master, corrupt=corrupt):
            run = json.loads(self.cmd("files", "download", dut, "--name", "dump.bin", "--output-dir", self.root / "downloads", "--session", s["id"]).stdout)["id"]
            result = self.formula_done(s["id"], run)
        self.assertEqual(result["run"]["state"], "completed", result)
        artifact = result["run"]["artifacts"][0]
        self.assertEqual(Path(artifact["path"]).read_bytes(), content)
        self.assertEqual(artifact["sha256"], hashlib.sha256(content).hexdigest())
        self.assertTrue(artifact["complete"])
        self.assertTrue(result["results"][-1]["verified"])
        self.assertEqual(len(hits), 2)
        self.assertIsNone(self.status(s["id"])["writer"])
        self.assertFalse((self.root / "SHOULD_NOT_EXIST").exists())
        self.assertFalse(list(self.root.glob("sericon-*")))

    def test_files_noise_exhaustion_keeps_only_verified_partial_chunks(self):
        s = self.start("--baud", "115200")
        dut = self.root / "binary"
        dut.write_bytes(bytes(range(256)) * 12)
        attempts = []
        def corrupt(command, output):
            if "skip=1 count=1" in command:
                attempts.append(command)
                return corrupt_chunk(output)
            return output
        with Shell(self.master, corrupt=corrupt):
            run = self.files_start(s["id"], dut)
            result = self.formula_done(s["id"], run)
        self.assertEqual(result["run"]["state"], "failed", result)
        self.assertIn("3 attempts", result["run"]["error"])
        artifact = result["run"]["artifacts"][0]
        self.assertFalse(artifact["complete"])
        self.assertTrue(artifact["path"].endswith(".partial"))
        self.assertEqual(Path(artifact["path"]).read_bytes(), dut.read_bytes()[:1024])
        self.assertEqual(len(attempts), 3)
        self.assertIsNone(self.status(s["id"])["writer"])

    def test_files_listing_and_hex_fallback_and_empty_download(self):
        s = self.start("--baud", "115200")
        dut = self.root / "DUT"
        dut.mkdir()
        for name in ["a'quote", "back\\slash", "two\nlines", ".hidden", "empty"]:
            (dut / name).write_bytes(b"")
        (dut / "directory").mkdir()
        (dut / "link").symlink_to(dut / "empty")
        bins = self.root / "bin"
        bins.mkdir()
        for name in ["od", "cksum", "dd"]:
            (bins / name).symlink_to("/usr/bin/" + name)
        with Shell(self.master, env=dict(os.environ, PATH=str(bins))):
            agent = self.agent()
            started = agent.tool("sericon_files_list", session=s["id"], path=str(dut))
            result = self.formula_done(s["id"], started["id"])
            self.assertEqual(result["run"]["state"], "completed", result)
            self.assertEqual({v["name"] for v in result["results"] if "name" in v}, {p.name for p in dut.iterdir()})
            self.assertEqual(result["results"][-1]["capabilities"]["encoding"], "hex")
            for name in ["empty", "empty"]:
                run = self.files_start(s["id"], dut / name)
                result = self.formula_done(s["id"], run)
                self.assertEqual(result["run"]["state"], "completed", result)
                self.assertEqual(Path(result["run"]["artifacts"][0]["path"]).read_bytes(), b"")
            self.assertEqual(len(list((self.root / "downloads").glob("formula-*/dump.bin"))), 2)
            run = self.files_start(s["id"], dut / "link")
            self.assertEqual(self.formula_done(s["id"], run)["run"]["state"], "failed")

    def test_files_cancel_and_takeover_stop_writes_and_keep_partial(self):
        import threading
        s = self.start("--baud", "115200")
        dut = self.root / "binary"
        dut.write_bytes(b"data" * 2048)
        paused = threading.Event()
        def before(command, shell):
            if "skip=1 count=1" in command:
                paused.set()
                shell.stop.wait(3)
        with Shell(self.master, before=before) as shell:
            run = self.files_start(s["id"], dut)
            self.assertTrue(paused.wait(4))
            reply = self.control(s["id"], {"op":"send","actor":"another","client_id":"another","data_base64":"WA=="})
            self.assertIn("input busy", reply["error"])
            self.control(s["id"], {"op":"claim","actor":"human","client_id":"human-fixture","takeover":True})
            result = self.formula_done(s["id"], run)
            self.assertEqual(result["run"]["state"], "cancelled", result)
            self.assertEqual(self.status(s["id"])["writer"]["client_id"], "human-fixture")
            self.assertFalse(result["run"]["artifacts"][0]["complete"])
            count = len(shell.commands)
            time.sleep(.15)
            self.assertEqual(count, len(shell.commands))

    def test_files_changed_source_rejected_at_final_verification(self):
        s = self.start("--baud", "115200")
        dut = self.root / "changing"
        dut.write_bytes(b"a" * 1500)
        changed = []
        def before(command, shell):
            if "skip=1 count=1" in command and not changed:
                dut.write_bytes(b"b" * 1500)
                changed.append(True)
        with Shell(self.master, before=before):
            run = self.files_start(s["id"], dut)
            result = self.formula_done(s["id"], run)
        self.assertEqual(result["run"]["state"], "failed", result)
        self.assertIn("whole-file checksum", result["run"]["error"])
        self.assertFalse(result["run"]["artifacts"][0]["complete"])

    def test_files_unsupported_shell_explains_missing_tools_without_output(self):
        s = self.start("--baud", "115200", "--no-log")
        with Shell(self.master, env=dict(os.environ, PATH="/nonexistent")):
            run = json.loads(self.cmd("files", "probe", "--session", s["id"]).stdout)["id"]
            result = self.formula_done(s["id"], run)
        self.assertEqual(result["run"]["state"], "failed", result)
        self.assertIn("existing cksum", result["run"]["error"])
        self.assertIsNone(self.status(s["id"])["writer"])
        self.assertEqual(result["run"]["artifacts"], [])

    def test_files_explicit_cancel_retains_input_during_unfinished_exchange(self):
        import threading
        s = self.start("--baud", "115200")
        paused = threading.Event()
        def before(command, shell):
            paused.set()
            shell.stop.wait(3)
        with Shell(self.master, before=before) as shell:
            run = json.loads(self.cmd("files", "probe", "--session", s["id"]).stdout)["id"]
            self.assertTrue(paused.wait(3))
            self.cmd("files", "cancel", run, "--session", s["id"])
            result = self.formula_done(s["id"], run)
            self.assertEqual(result["run"]["state"], "cancelled", result)
            self.assertTrue(result["run"]["writer_retained"])
            self.assertEqual(len(shell.commands), 1)

    def test_files_busybox_applets_and_mcp_download(self):
        import shutil
        busybox = shutil.which("busybox")
        if not busybox:
            self.skipTest("optional BusyBox fixture is not installed")
        s = self.start("--baud", "115200", "--no-log")
        bins = self.root / "bin"
        bins.mkdir()
        (bins / "busybox").symlink_to(busybox)
        dut = self.root / "image"
        content = bytes(range(256)) * 7
        dut.write_bytes(content)
        corrupted = []
        def corrupt(command, output):
            if "skip=0 count=1" in command and not corrupted:
                corrupted.append(True)
                return corrupt_chunk(output)
            return output
        with Shell(self.master, env=dict(os.environ, PATH=str(bins)), corrupt=corrupt):
            agent = self.agent()
            run = agent.tool("sericon_files_download", session=s["id"], path=str(dut),
                             name="image.bin", output_dir=str(self.root / "downloads"))
            result = self.formula_done(s["id"], run["id"])
            self.assertEqual(result["run"]["state"], "completed", result)
            self.assertEqual(result["results"][-1]["capabilities"]["encoder"], "busybox base64")
            self.assertTrue(corrupted)
            view = agent.tool("sericon_formula_read", session=s["id"], run=run["id"])
            self.assertEqual(Path(view["run"]["artifacts"][0]["path"]).read_bytes(), content)

    def test_files_rejects_pseudo_files_aliases_and_unsafe_host_names(self):
        s = self.start("--baud", "115200")
        alias = self.root / "proc-alias"
        alias.symlink_to("/proc", target_is_directory=True)
        dut = self.root / "small"
        dut.write_text("data")
        with Shell(self.master) as shell:
            for path, name in [(alias / "version", "file"), ("/dev/zero", "file"), (dut, ".."), (dut, "../escape")]:
                run = self.files_start(s["id"], path, name)
                result = self.formula_done(s["id"], run)
                self.assertEqual(result["run"]["state"], "failed", result)
                self.assertFalse(result["run"]["artifacts"])
                self.assertIsNone(self.status(s["id"])["writer"])
            self.assertFalse(any("skip=" in command for command in shell.commands))
        self.assertFalse((self.root / "downloads").exists())

    def test_files_menu_is_opt_in_and_navigation_does_not_reach_uart(self):
        s = self.start("--baud", "115200")
        p, terminal, slave, original = self.terminal("attach", s["id"])
        self.wait_terminal(terminal, b"sericon")
        os.write(terminal, b"\x1dm" + b"\x1b[B" * 4 + b"\r")
        self.wait_terminal(terminal, b"Start at an empty Linux shell prompt")
        self.assertFalse(select.select([self.master], [], [], .1)[0])
        os.write(terminal, b"d\x15/etc/test\x03\x1dmq")
        self.wait_terminal(terminal, b"Embedded Linux / Files")
        self.assertFalse(select.select([self.master], [], [], .1)[0])
        os.write(terminal, b"q\x1dd")
        p.wait(timeout=5)
        self.assertEqual(termios.tcgetattr(slave), original)

    def test_files_browser_download_hint_default_location_and_back(self):
        helper=self.current_helper()
        s=self.start('--baud','115200','--no-log')
        dut=self.root/'dut'; dut.mkdir()
        (dut/'a-dir').mkdir()
        source=dut/'b-image.bin'; content=bytes(range(256))*5; source.write_bytes(content)
        p,terminal,slave,original=self.terminal('attach',s['id'])
        self.wait_terminal(terminal,b'sericon')
        with Shell(self.master) as shell:
            os.write(terminal,b'\x1dlh'+helper.encode()+b'\rg\x15'+str(dut).encode()+b'\r')
            self.wait_terminal(terminal,b'1 folders loaded')
            os.write(terminal,b'\x1b[B\x1b[C')
            self.wait_terminal(terminal,b'0 entries')
            os.write(terminal,b'\x1b[D')  # Collapse the empty directory in place.
            self.wait_terminal(terminal,b'Enter expand')
            os.write(terminal,b'\x1b[B')
            self.wait_terminal(terminal,b'Enter download')
            count=len(shell.commands)
            os.write(terminal,b'\r')
            output=self.wait_terminal(terminal,b'Use ./downloads (default)')
            self.assertIn(str(self.root/'downloads').encode(),output)
            os.write(terminal,b'\x1b[B\r')
            self.wait_terminal(terminal,b'Path:')
            os.write(terminal,b'./discarded\x1b')
            self.wait_terminal(terminal,b'Use ./downloads (default)')
            os.write(terminal,b'\x1b[F\r')
            output=self.wait_terminal(terminal,b'Files tree')
            self.assertIn(b'b-image.bin',output)
            self.assertIn(b'Enter download',output)
            self.assertEqual(len(shell.commands),count)  # Choosing/cancelling a location stays local.
            os.write(terminal,b'\r')
            self.wait_terminal(terminal,b'Use ./downloads (default)')
            os.write(terminal,b'\r')
            runs=eventually(lambda:[r for r in self.control(s['id'],{'op':'formula_runs'})['result']
                                   if r['formula']=='linux-download'])
            result=self.formula_done(s['id'],runs[-1]['id'])
            self.assertEqual(result['run']['state'],'completed',result)
            artifact=result['run']['artifacts'][0]
            saved=Path(artifact['path'])
            self.assertEqual(saved.parent.parent,self.root/'downloads')
            self.assertEqual(saved.read_bytes(),content)
            self.assertTrue(artifact['complete'])
            self.assertEqual(artifact['sha256'],hashlib.sha256(content).hexdigest())
            self.assertFalse((self.root/'discarded').exists())
        os.write(terminal,b'\x1dd');p.wait(timeout=5)
        self.assertEqual(termios.tcgetattr(slave),original)

    def test_files_download_custom_host_path_empty_validation_and_cancel(self):
        s=self.start('--baud','115200','--no-log')
        source=self.root/'firmware.bin'; content=b'firmware\x00\xff\r\n'; source.write_bytes(content)
        p,terminal,slave,original=self.terminal('attach',s['id'])
        self.wait_terminal(terminal,b'sericon')
        os.write(terminal,b'\x1dld\x15'+str(source).encode()+b'\r')
        self.wait_terminal(terminal,b'Use ./downloads (default)')
        os.write(terminal,b'\x1b[F\r')
        output=self.wait_terminal(terminal,b'DUT path:')
        self.assertIn(str(source).encode(),output)
        os.write(terminal,b'\r')
        self.wait_terminal(terminal,b'Use ./downloads (default)')
        os.write(terminal,b'\x1b[B\r')
        output=self.wait_terminal(terminal,b'Path:')
        self.assertIn(b'Enter a download folder on this host',output)
        self.assertNotIn(str(self.root/'downloads').encode(),output.rsplit(b'\x1b[2J',1)[-1])
        os.write(terminal,b'\r')
        self.wait_terminal(terminal,b'Enter a host folder, for example ./captures')
        self.assertFalse(any(e['kind']=='tx' for e in self.read(s['id'])['events']))
        with Shell(self.master):
            os.write(terminal,b'./custom captures\r')  # Relative host path, without clearing a default.
            self.wait_terminal(terminal,b'Download verified')  # Drain prompt redraws like a real terminal.
            runs=eventually(lambda:self.control(s['id'],{'op':'formula_runs'})['result'])
            result=self.formula_done(s['id'],runs[-1]['id'])
            self.assertEqual(result['run']['state'],'completed',result)
            saved=Path(result['run']['artifacts'][0]['path'])
            self.assertEqual(saved.parent.parent,self.root/'custom captures')
            self.assertEqual(saved.read_bytes(),content)
            self.assertTrue(result['run']['artifacts'][0]['complete'])
            self.assertFalse((self.root/'downloads').exists())
            self.assertIsNone(self.status(s['id'])['writer'])
        os.write(terminal,b'\x1dd');p.wait(timeout=5)
        self.assertEqual(termios.tcgetattr(slave),original)

    def test_file_tree_lazy_scan_refresh_and_links(self):
        helper=self.current_helper()
        s=self.start('--baud','115200','--no-log')
        dut=self.root/'tree';dut.mkdir()
        a=dut/'a';a.mkdir(); nested=a/'nested';nested.mkdir()
        (nested/'config.json').write_text('{}')
        z=dut/'z';z.mkdir();(z/'firmware.bin').write_bytes(b'fixture')
        (dut/'loop').symlink_to(dut,target_is_directory=True)
        p,terminal,slave,original=self.terminal('attach',s['id'])
        self.wait_terminal(terminal,b'sericon')
        with Shell(self.master) as shell:
            os.write(terminal,b'\x1dlh'+helper.encode()+b'\rg\x15'+str(dut).encode()+b'\r')
            self.wait_terminal(terminal,b'1 folders loaded')
            initial=len(shell.commands)
            self.assertFalse(any(str(nested) in c.replace("''", "") for c in shell.commands))
            os.write(terminal,b'\x1b[B\x1b[C')
            output=self.wait_terminal(terminal,b'2 folders loaded')
            self.assertIn(b'nested/',output)
            self.assertGreater(len(shell.commands),initial)
            count=len(shell.commands)
            os.write(terminal,b'\x1b[D\x1b[C')  # Reopening a cached folder sends nothing.
            self.wait_terminal(terminal,b'Enter collapse')
            self.assertEqual(len(shell.commands),count)
            os.write(terminal,b'o')
            self.wait_terminal(terminal,b'Tree options')
            os.write(terminal,b'\x1b[B\r')
            self.wait_terminal(terminal,b'Scan complete within tree limits')
            self.assertTrue(any(str(nested) in c.replace("''", "") for c in shell.commands))
            self.assertFalse(any("list '"+str(dut/'loop')+"'" in c.replace("''", "") for c in shell.commands))
            count=len(shell.commands)
            os.write(terminal,b'\x1b[C\x1b[C')  # Enter the now-cached nested folder.
            self.wait_terminal(terminal,b'config.json')
            self.assertEqual(len(shell.commands),count)
            (nested/'new.sh').write_text('#!/bin/sh\n')
            os.write(terminal,b'o')
            self.wait_terminal(terminal,b'Tree options')
            os.write(terminal,b'\x1b[B\x1b[B\r')
            self.wait_terminal(terminal,b'new.sh')
            self.assertGreater(len(shell.commands),count)
            self.assertIsNone(self.status(s['id'])['writer'])
            os.write(terminal,b'q')
            self.wait_terminal(terminal,b'Return to file tree')
            count=len(shell.commands)
            os.write(terminal,b'\r')
            self.wait_terminal(terminal,b'new.sh')
            self.assertEqual(len(shell.commands),count)
        os.write(terminal,b'\x1dd');p.wait(timeout=5)
        self.assertEqual(termios.tcgetattr(slave),original)

    def test_file_tree_scan_stop_finishes_current_directory_without_further_commands(self):
        import threading
        self.env['NO_COLOR']='1'
        helper=self.current_helper()
        s=self.start('--baud','115200','--no-log')
        dut=self.root/'tree';dut.mkdir()
        for name in ('a','b','c'): (dut/name).mkdir()
        entered=threading.Event();release=threading.Event()
        def before(command,shell):
            if " list '"+str(dut/'a')+"'" in command.replace("''", ""):
                entered.set();release.wait(4)
        p,terminal,slave,original=self.terminal('attach',s['id'])
        self.wait_terminal(terminal,b'sericon')
        with Shell(self.master,before=before) as shell:
            os.write(terminal,b'\x1dlh'+helper.encode()+b'\rg\x15'+str(dut).encode()+b'\r')
            self.wait_terminal(terminal,b'1 folders loaded')
            os.write(terminal,b'o')
            self.wait_terminal(terminal,b'Tree options')
            os.write(terminal,b'\x1b[B\r')
            self.wait_terminal(terminal,b'Scanning')
            self.assertTrue(entered.wait(3))
            os.write(terminal,b'c')
            self.wait_terminal(terminal,b'Stopping after current directory')
            release.set()
            self.wait_terminal(terminal,b'2 folders loaded')
            count=len(shell.commands)
            time.sleep(.4)
            self.assertEqual(len(shell.commands),count)
            self.assertFalse(any(" list '"+str(dut/'b')+"'" in c.replace("''", "") for c in shell.commands))
            self.assertIsNone(self.status(s['id'])['writer'])
            fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',12,40,0,0))
            output=self.wait_terminal(terminal,b'Files tree')
            self.assertNotRegex(output,rb'\x1b\[[0-9;]*(?:3[0-7]|4[0-7]|9[0-7])m')
            for row,col in re.findall(rb'\x1b\[(\d+);(\d+)H',output):
                self.assertLessEqual(int(row),12);self.assertLessEqual(int(col),40)
        os.write(terminal,b'\x1dd');p.wait(timeout=5)
        self.assertEqual(termios.tcgetattr(slave),original)

    def test_file_tree_unreadable_folder_can_be_refreshed(self):
        helper=self.current_helper()
        s=self.start('--baud','115200','--no-log')
        dut=self.root/'tree';dut.mkdir();gone=dut/'gone';gone.mkdir()
        p,terminal,slave,original=self.terminal('attach',s['id'])
        self.wait_terminal(terminal,b'sericon')
        with Shell(self.master):
            os.write(terminal,b'\x1dlh'+helper.encode()+b'\rg\x15'+str(dut).encode()+b'\r')
            self.wait_terminal(terminal,b'1 folders loaded')
            gone.rmdir()
            os.write(terminal,b'\x1b[B\x1b[C')
            self.wait_terminal(terminal,b'1 unavailable')
            gone.mkdir();(gone/'restored.txt').write_text('restored')
            os.write(terminal,b'o')
            self.wait_terminal(terminal,b'Tree options')
            os.write(terminal,b'\x1b[B\x1b[B\r')
            self.wait_terminal(terminal,b'restored.txt')
            self.assertIsNone(self.status(s['id'])['writer'])
        os.write(terminal,b'\x1dd');p.wait(timeout=5)
        self.assertEqual(termios.tcgetattr(slave),original)

    def test_helper_cancel_mid_upload_retains_writer_and_never_executes(self):
        s=self.start('--baud','115200')
        if not json.loads(self.cmd('files','helpers').stdout): self.skipTest('no bundled payloads')
        cancelled=[]
        def before(command,shell):
            if "/p0001'" in command:
                run=self.status(s['id'])['writer']['actor'].split(':',1)[1]
                self.control(s['id'],{'op':'formula_cancel','run':run})
                cancelled.append(run)
                shell.stop.set()
        with Shell(self.master,before=before) as shell:
            run=json.loads(self.cmd('files','helper-install','--arch','x86_64','--directory',self.root).stdout)['id']
            result=self.formula_done(s['id'],run,timeout=10)
        self.assertEqual(cancelled,[run])
        self.assertEqual(result['run']['state'],'cancelled',result)
        self.assertTrue(result['run']['writer_retained'])
        self.assertFalse(any('chmod 700' in c or '"$t" info' in c for c in shell.commands))
        self.assertFalse(Path(result['results'][0]['helper']).exists())
        last=self.status(s['id'])['latest_cursor']
        self.control(s['id'],{'op':'claim','actor':'human','client_id':'human','takeover':True})
        self.control(s['id'],{'op':'release','client_id':'human'})
        time.sleep(.1)
        self.assertFalse(any(e['kind']=='tx' for e in self.read(s['id'],last)['events']))

    def test_helper_menu_selection_stays_local_and_restores_terminal(self):
        s=self.start('--baud','115200')
        p,terminal,slave,original=self.terminal('attach',s['id'])
        self.wait_terminal(terminal,b'sericon')
        os.write(terminal,b'\x1dlh')
        self.wait_terminal(terminal,b'Helper path:')
        os.write(terminal,b'/var/tmp/example/helper\x1dm')
        self.wait_terminal(terminal,b'sericon  /  Menu')
        os.write(terminal,b'q')
        self.wait_terminal(terminal,b'/var/tmp/example/helper')
        os.write(terminal,b'\r')
        self.wait_terminal(terminal,b'Backend selected')
        self.assertFalse(any(e['kind']=='tx' for e in self.read(s['id'])['events']))
        os.write(terminal,b'q\x1dd')
        p.wait(timeout=5)
        self.assertEqual(termios.tcgetattr(slave),original)

    def current_helper(self):
        helper=Path(__file__).resolve().parents[1]/'target/helpers/x86_64'
        if not helper.exists(): self.skipTest('build native helper first')
        return str(helper)

    def test_shared_option_menus_arrows_escape_resize_and_uart_isolation(self):
        self.env['NO_COLOR']='1'
        s=self.start('--baud','115200','--no-log')
        p,terminal,slave,original=self.terminal('attach',s['id'])
        self.wait_terminal(terminal,b'Ctrl-]')
        os.write(terminal,b'\x1dl\x1b[B\r')
        output=self.wait_terminal(terminal,b'Helper setup')
        self.assertIn('╭'.encode(),output)
        os.write(terminal,b'\r')
        self.wait_terminal(terminal,b'Helper architecture')
        os.write(terminal,b'\x1b')
        self.wait_terminal(terminal,b'Helper setup')
        os.write(terminal,b'\x1b[B\r')
        self.wait_terminal(terminal,b'Helper path:')
        os.write(terminal,b'/var/tmp/unchanged\x1b[<65;10;5M\x1b')
        self.wait_terminal(terminal,b'Helper setup')
        os.write(terminal,b'\x1b')
        self.wait_terminal(terminal,b'Connect and browse')
        fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',12,40,0,0))
        os.write(terminal,b'\x1b[F')
        output=self.wait_terminal(terminal,b'Back to live terminal')
        self.assertIn('›'.encode(),output)
        self.assertNotRegex(output,rb'\x1b\[[0-9;]*(?:3[0-7]|4[0-7]|90)m')
        for row,col in re.findall(rb'\x1b\[(\d+);(\d+)H',output):
            self.assertLessEqual(int(row),12);self.assertLessEqual(int(col),40)
        os.write(terminal,b'\r\x1dfcode=\\d+\x1b')
        self.wait_terminal(terminal,b'Search options')
        os.write(terminal,b'\x1d?')
        output=self.wait_terminal(terminal,b'Search help')
        self.assertNotIn(b'/  Search options',output.rsplit(b'\x1b[2J',1)[-1])
        os.write(terminal,b'q')
        self.wait_terminal(terminal,b'Search options')
        os.write(terminal,b'\x1b[B'*4+b'\r')
        self.wait_terminal(terminal,b'[regex] /code=')
        os.write(self.master,b'code=503\r\n')
        os.write(terminal,b'\r')
        self.wait_terminal(terminal,b'Match 1')
        os.write(terminal,b'\x1b')
        self.wait_terminal(terminal,b'Search options')
        os.write(terminal,b'\x1b[F\r')
        self.wait_terminal(terminal,b'code=503')
        self.assertFalse(any(e['kind']=='tx' for e in self.read(s['id'])['events']))
        os.write(terminal,b'\x1dd');p.wait(timeout=5)
        self.assertEqual(termios.tcgetattr(slave),original)

    def test_formula_settings_menu_edits_typed_fields_without_json(self):
        s=self.start('--baud','115200','--no-log')
        agent=self.agent()
        definition={'name':'aaa-menu','description':'Menu field fixture','source':'emit(#{count:params.count,enabled:params.enabled,label:params.label});',
                    'parameters':{'count':{'type':'integer','default':1},'enabled':{'type':'boolean','default':False},'label':{'type':'string','default':'original'}}}
        result=agent.tool('sericon_formula_put',formula=definition)
        self.assertNotIn('error',result)
        p,terminal,slave,original=self.terminal('attach',s['id'])
        self.wait_terminal(terminal,b'Ctrl-]')
        os.write(terminal,b'\x1dx\r')
        self.wait_terminal(terminal,b'Run formula')
        os.write(terminal,b'\x1b[B\r\x15false\r')
        self.wait_terminal(terminal,b'Expected integer')
        os.write(terminal,b'\x153\r')
        self.wait_terminal(terminal,b'count: 3')
        os.write(terminal,b'\x1b[B\r')
        self.wait_terminal(terminal,b'enabled: true')
        os.write(terminal,b'\x1b[B\r\x15discard\x1b')
        self.wait_terminal(terminal,b'label: original')
        os.write(terminal,b'\r\x15'+ '界 kept'.encode()+b'\x1b[<65;10;5M\r')
        self.wait_terminal(terminal,'label: 界 kept'.encode())
        os.write(terminal,b'\x1dm')
        self.wait_terminal(terminal,b'/  Menu')
        os.write(terminal,b'q')
        self.wait_terminal(terminal,'label: 界 kept'.encode())
        os.write(terminal,b'\x1b[H\r')
        runs=eventually(lambda:self.control(s['id'],{'op':'formula_runs'})['result'])
        result=self.formula_done(s['id'],runs[-1]['id'])
        self.assertEqual(result['run']['state'],'completed',result)
        self.assertEqual(result['results'],[{'count':3,'enabled':True,'label':'界 kept'}])
        self.assertFalse(any(e['kind']=='tx' for e in self.read(s['id'])['events']))
        os.write(terminal,b'\x1dd');p.wait(timeout=5)
        self.assertEqual(termios.tcgetattr(slave),original)

    def helper_menu_review(self, terminal, directory):
        os.write(terminal,b'a')
        self.wait_terminal(terminal,b'Helper architecture')
        os.write(terminal,b'\r')  # First bundled payload is x86_64, not the host/DUT auto-detected ABI.
        self.wait_terminal(terminal,b'Use /var/tmp')
        os.write(terminal,b'\x1b[B\r')
        self.wait_terminal(terminal,b'Path:')
        os.write(terminal,str(directory).encode()+b'\r')
        self.wait_terminal(terminal,b'Install helper')

    def test_files_menu_installs_and_selects_verified_helper(self):
        self.current_helper()
        s=self.start('--baud','115200','--no-log')
        inventory=self.control(s['id'],{'op':'helper_inventory'})['result']
        self.assertEqual(inventory,json.loads(self.cmd('files','helpers').stdout))
        self.assertEqual(inventory[0]['arch'],'x86_64')
        p,terminal,slave,original=self.terminal('attach',s['id'])
        self.wait_terminal(terminal,b'sericon')
        os.write(terminal,b'\x1dm'+b'\x1b[B'*4+b'\r')
        self.wait_terminal(terminal,b'Connect and browse')
        os.write(terminal,b'\x1b[B\r')
        self.wait_terminal(terminal,b'Helper setup')
        os.write(terminal,b'\r')
        self.wait_terminal(terminal,b'Helper architecture')
        # Every bundled architecture must be reachable and survive Back navigation.
        for payload in inventory[1:]:
            os.write(terminal,b'\x1b[B')
            self.wait_terminal(terminal,payload['abi'].encode())
            os.write(terminal,b'\r')
            self.wait_terminal(terminal,b'Use /var/tmp')
            os.write(terminal,b'\x1b[F\r')  # Back to architecture keeps its selection.
            self.wait_terminal(terminal,payload['abi'].encode())
        os.write(terminal,b'\x1b[H')
        self.wait_terminal(terminal,inventory[0]['abi'].encode())
        os.write(terminal,b'\r')
        self.wait_terminal(terminal,b'Use /var/tmp')
        os.write(terminal,b'\r')
        review=self.wait_terminal(terminal,b'Install helper')
        self.assertIn(b'/var/tmp',review)
        self.assertFalse(any(e['kind']=='tx' for e in self.read(s['id'])['events']))
        os.write(terminal,b'\x1b[B\r')  # Change install location.
        self.wait_terminal(terminal,b'Use /var/tmp')
        os.write(terminal,b'\x1b[B\r')
        prompt=self.wait_terminal(terminal,b'Path:')
        self.assertIn(b'Type a path, then Enter',prompt)
        self.assertNotIn(b'/var/tmp',prompt.rsplit(b'\x1b[2J',1)[-1])
        os.write(terminal,b'/discarded-draft\x1b')
        self.wait_terminal(terminal,b'Use /var/tmp')
        os.write(terminal,b'\x1b[B\r')
        prompt=self.wait_terminal(terminal,b'Path:')
        self.assertNotIn(b'/discarded-draft',prompt.rsplit(b'\x1b[2J',1)[-1])
        os.write(terminal,b'relative\r')
        self.wait_terminal(terminal,b'Use an absolute DUT directory')
        os.write(terminal,b'\x15'+str(self.root).encode()+b'\r')
        self.wait_terminal(terminal,b'Install helper')
        os.write(terminal,b'b')
        self.wait_terminal(terminal,b'Use '+str(self.root).encode())
        os.write(terminal,b'\r\x1dm')
        self.wait_terminal(terminal,b'sericon  /  Menu')
        os.write(terminal,b'q')
        self.wait_terminal(terminal,b'Install helper')
        self.assertFalse(any(e['kind']=='tx' for e in self.read(s['id'])['events']))
        with Shell(self.master):
            os.write(terminal,b'\r')
            runs=eventually(lambda:self.control(s['id'],{'op':'formula_runs'})['result'])
            result=self.formula_done(s['id'],runs[-1]['id'],timeout=30)
            self.assertEqual(result['run']['state'],'completed',result)
            helper=result['results'][-1]
            self.assertTrue(helper['verified_before_execution'])
            self.assertEqual(Path(helper['helper']).parent.parent,self.root)
            self.assertEqual(hashlib.sha256(Path(helper['helper']).read_bytes()).hexdigest(),inventory[0]['sha256'])
            self.wait_terminal(terminal,b'Helper installed and selected')
            os.write(terminal,b'i')  # No manual h/path selection needed.
            self.wait_terminal(terminal,b'Device inspector')
            self.assertIsNone(self.status(s['id'])['writer'])
        os.write(terminal,b'q\x1dd')
        p.wait(timeout=5)
        self.assertEqual(termios.tcgetattr(slave),original)

    def test_files_menu_failed_install_preserves_selected_helper(self):
        self.current_helper()
        s=self.start('--baud','115200','--no-log')
        p,terminal,slave,original=self.terminal('attach',s['id'])
        self.wait_terminal(terminal,b'sericon')
        os.write(terminal,b'\x1dlh/var/tmp/previous/helper\r')
        self.wait_terminal(terminal,b'Backend selected')
        self.helper_menu_review(terminal,self.root)
        def corrupt(command,output):
            return corrupt_chunk(output) if '/p0000\'' in command else output
        with Shell(self.master,corrupt=corrupt):
            os.write(terminal,b'\r')
            runs=eventually(lambda:self.control(s['id'],{'op':'formula_runs'})['result'])
            result=self.formula_done(s['id'],runs[-1]['id'],timeout=30)
            self.assertEqual(result['run']['state'],'failed',result)
            self.assertTrue(result['results'][0]['helper'])  # Staging path must never be selected.
            self.wait_terminal(terminal,b'failed')
            os.write(terminal,b'h')
            self.wait_terminal(terminal,b'/var/tmp/previous/helper')
        os.write(terminal,b'\x1dd')
        p.wait(timeout=5)
        self.assertEqual(termios.tcgetattr(slave),original)

    def test_files_menu_cancel_install_preserves_selection_and_job_guard(self):
        import threading
        self.current_helper()
        s=self.start('--baud','115200','--no-log')
        p,terminal,slave,original=self.terminal('attach',s['id'])
        self.wait_terminal(terminal,b'sericon')
        os.write(terminal,b'\x1dlh/var/tmp/previous/helper\r')
        self.wait_terminal(terminal,b'Backend selected')
        self.helper_menu_review(terminal,self.root)
        paused=threading.Event()
        def before(command,shell):
            paused.set()
            shell.stop.wait(4)
        with Shell(self.master,before=before):
            os.write(terminal,b'\r')
            self.assertTrue(paused.wait(3))
            os.write(terminal,b'a')
            self.wait_terminal(terminal,b'Wait for this job')
            os.write(terminal,b'\r\x1b[B\r')
            self.wait_terminal(terminal,b'cancelled')
            runs=self.control(s['id'],{'op':'formula_runs'})['result']
            self.assertEqual(len(runs),1)
            self.assertEqual(runs[0]['state'],'cancelled')
            os.write(terminal,b'h')
            self.wait_terminal(terminal,b'/var/tmp/previous/helper')
        os.write(terminal,b'\x1dd')
        p.wait(timeout=5)
        self.assertEqual(termios.tcgetattr(slave),original)

    def test_upload_mcp_noise_snapshot_and_no_replacement(self):
        s=self.start('--baud','115200','--no-log')
        helper=self.current_helper()
        source=self.root/'host.bin'; data=bytes(range(256))*13+b'\x00end'; source.write_bytes(data)
        target=self.root/"DUT ' $() file"
        corruptions=[]
        def before(command,shell):
            if 'upload-begin' in command: source.write_bytes(b'changed after snapshot')
        def corrupt(command,output):
            if 'upload-write' in command and not corruptions:
                corruptions.append(True); return corrupt_chunk(output)
            if 'upload-commit' in command and len(corruptions)==1:
                corruptions.append(True); return corrupt_chunk(output)
            return output
        with Shell(self.master,before=before,corrupt=corrupt) as shell:
            agent=self.agent()
            run=agent.tool('sericon_files_upload',session=s['id'],source=str(source),path=str(target),helper=helper,executable=True)['id']
            result=self.formula_done(s['id'],run)
            self.assertEqual(result['run']['state'],'completed',result)
            self.assertEqual(target.read_bytes(),data)
            self.assertEqual(target.stat().st_mode&0o777,0o700)
            self.assertEqual(len(corruptions),2)
            self.assertEqual(result['results'][-1]['sha256'],hashlib.sha256(data).hexdigest())
            self.assertFalse(list(self.root.glob('.sericon-upload-*.partial')))
            self.assertIsNone(self.status(s['id'])['writer'])
            run=json.loads(self.cmd('files','upload',source,target,'--helper',helper,'--session',s['id']).stdout)['id']
            failed=self.formula_done(s['id'],run)
            self.assertEqual(failed['run']['state'],'failed',failed)
            self.assertIn('destination exists',failed['run']['error'])
            self.assertEqual(target.read_bytes(),data)
            self.assertTrue(any('upload-commit' in c for c in shell.commands))

    def test_upload_rejects_sources_before_uart_and_old_helper_supports_reads(self):
        s=self.start('--baud','115200')
        helper=self.current_helper()
        source=self.root/'source'; source.write_bytes(b'hello')
        run=self.formula_start(s['id'],'linux_upload("/missing-host-source", "/tmp/new", "/tmp/helper", false);')
        result=self.formula_done(s['id'],run)
        self.assertIn('requires interactive=true',result['run']['error'])
        link=self.root/'link'; link.symlink_to(source)
        large=self.root/'large'
        with large.open('wb') as f: f.truncate(64*1024*1024+1)
        for bad in (link,large,self.root):
            run=json.loads(self.cmd('files','upload',bad,self.root/'new','--helper',helper,'--session',s['id']).stdout)['id']
            result=self.formula_done(s['id'],run)
            self.assertEqual(result['run']['state'],'failed',result)
        self.assertFalse(any(e['kind']=='tx' for e in self.read(s['id'])['events']))
        import subprocess,shlex
        info=b'sericon-helper/1 x86_64 check read list'
        crc,length=subprocess.check_output(['cksum'],input=info).split()
        old=self.root/'old-helper'
        old.write_text('#!/bin/sh\nif [ "$2" = info ]; then\nprintf "%s\\n%s:C\\n%s %s\\n" '+shlex.quote(info.hex())+' "$1" '+crc.decode()+' '+length.decode()+'\nelse\nexec '+shlex.quote(helper)+' "$@"\nfi\n')
        old.chmod(0o700)
        with Shell(self.master) as shell:
            run=json.loads(self.cmd('files','probe','--helper',old,'--session',s['id']).stdout)['id']
            self.assertEqual(self.formula_done(s['id'],run)['run']['state'],'completed')
            run=json.loads(self.cmd('files','upload',source,self.root/'new','--helper',old,'--session',s['id']).stdout)['id']
            result=self.formula_done(s['id'],run)
            self.assertEqual(result['run']['state'],'failed',result)
            self.assertIn('lacks upload',result['run']['error'])
            self.assertFalse(any('upload-begin' in c for c in shell.commands))
        self.assertIsNone(self.status(s['id'])['writer'])

    def test_upload_noise_failure_retains_partial_and_cancel_revokes_writes(self):
        s=self.start('--baud','115200','--no-log'); helper=self.current_helper()
        source=self.root/'source'; data=bytes(range(256))*16; source.write_bytes(data)
        dest=self.root/'destination'
        with Shell(self.master,corrupt=lambda c,o: corrupt_chunk(o) if 'upload-write' in c else o) as shell:
            run=json.loads(self.cmd('files','upload',source,dest,'--helper',helper,'--session',s['id']).stdout)['id']
            result=self.formula_done(s['id'],run)
            self.assertEqual(result['run']['state'],'failed',result)
            self.assertFalse(dest.exists())
            stage=Path(result['results'][0]['partial_path'])
            self.assertEqual(stage.read_bytes(),data[:1024])
            self.assertEqual(sum('upload-write' in c for c in shell.commands),3)
            self.assertFalse(any('upload-commit' in c for c in shell.commands))
            self.assertIsNone(self.status(s['id'])['writer'])
        import threading
        entered=threading.Event(); proceed=threading.Event()
        def before(command,shell):
            if 'upload-write' in command: entered.set(); proceed.wait(2)
        with Shell(self.master,before=before) as shell:
            run=json.loads(self.cmd('files','upload',source,dest,'--helper',helper,'--session',s['id']).stdout)['id']
            self.assertTrue(entered.wait(3))
            self.control(s['id'],{'op':'claim','actor':'human','client_id':'human','takeover':True})
            self.control(s['id'],{'op':'release','client_id':'human'})
            proceed.set()
            result=self.formula_done(s['id'],run)
            self.assertEqual(result['run']['state'],'cancelled',result)
            time.sleep(.1)
            self.assertEqual(sum('upload-write' in c for c in shell.commands),1)
            self.assertFalse(any('upload-commit' in c for c in shell.commands))
            self.assertFalse(dest.exists())

    def test_device_inspector_collection_formula_and_mcp_artifact_hashes(self):
        s=self.start('--baud','115200','--no-log'); helper=self.current_helper()
        corrupted=[]
        def corrupt(command,output):
            if 'inspect identity' in command and not corrupted:
                corrupted.append(True);return corrupt_chunk(output)
            return output
        with Shell(self.master,corrupt=corrupt):
            agent=self.agent()
            run=agent.tool('sericon_files_inspect',session=s['id'],helper=helper)['id']
            result=self.formula_done(s['id'],run)
            self.assertEqual(result['run']['state'],'completed',result)
            sections=[r for r in result['results'] if 'section' in r]
            self.assertEqual(len(sections),7)
            self.assertIn(['helper_arch','x86_64'],sections[0]['values'])
            self.assertTrue(corrupted)
            self.assertFalse(result['run']['artifacts'])
            run=agent.tool('sericon_files_collect_overview',session=s['id'],helper=helper,output_dir=str(self.root/'collections'))['id']
            result=self.formula_done(s['id'],run)
            self.assertEqual(result['run']['state'],'completed',result)
            artifacts={Path(a['path']).name:a for a in result['run']['artifacts']}
            self.assertEqual(set(artifacts),{'device-overview.json','manifest.json'})
            for a in artifacts.values():
                p=Path(a['path']); self.assertEqual(p.stat().st_mode&0o777,0o600)
                self.assertTrue(a['complete']); self.assertEqual(hashlib.sha256(p.read_bytes()).hexdigest(),a['sha256'])
            overview=json.loads(Path(artifacts['device-overview.json']['path']).read_text())
            manifest=json.loads(Path(artifacts['manifest.json']['path']).read_text())
            self.assertEqual(manifest['files'][0]['sha256'],artifacts['device-overview.json']['sha256'])
            self.assertFalse(overview['summary']['atomic_snapshot'])
            for section in overview['sections']:
                raw=base64.b64decode(section['raw_base64'])
                self.assertEqual(hashlib.sha256(raw).hexdigest(),section['sha256'])
            run=json.loads(self.cmd('formulas','run','linux-collect-overview','--params',json.dumps({'helper':helper}),'--output-dir',self.root/'formula-collection','--session',s['id']).stdout)['id']
            self.assertEqual(self.formula_done(s['id'],run)['run']['state'],'completed')
            self.assertIsNone(self.status(s['id'])['writer'])

    def test_files_upload_inspector_and_collection_menu(self):
        s=self.start('--baud','115200','--no-log'); helper=self.current_helper()
        source=self.root/'source';source.write_bytes(b'from menu\0\xff')
        dest=self.root/'uploaded'
        p,terminal,slave,original=self.terminal('attach',s['id'])
        self.wait_terminal(terminal,b'Ctrl-]')
        os.write(terminal,b'\x1dlh'+helper.encode()+b'\r')
        self.wait_terminal(terminal,b'Backend selected')
        os.write(terminal,b'u');self.wait_terminal(terminal,b'Host file:')
        os.write(terminal,str(source).encode()+b'\r');self.wait_terminal(terminal,b'DUT file:')
        os.write(terminal,b'\x15'+str(dest).encode()+b'\r');self.wait_terminal(terminal,b'Upload file')
        self.assertFalse(any(e['kind']=='tx' for e in self.read(s['id'])['events']))
        with Shell(self.master):
            os.write(terminal,b'\x1b[B\r\x1b[A\r');self.wait_terminal(terminal,b'upload verified')
            self.assertEqual(dest.read_bytes(),source.read_bytes())
            self.assertEqual(dest.stat().st_mode&0o777,0o700)
            os.write(terminal,b'i');self.wait_terminal(terminal,b'Device inspector')
            os.write(terminal,b'\x1b[B\r');self.wait_terminal(terminal,b'cpu (2/7)')
            fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',12,40,0,0))
            os.write(terminal,b'j\x1dm');self.wait_terminal(terminal,b'Menu')
            os.write(terminal,b'q');self.wait_terminal(terminal,b'Device inspector')
            os.write(terminal,b's');self.wait_terminal(terminal,b'Host directory:')
            os.write(terminal,b'\x15'+str(self.root/'collections').encode()+b'\r')
            self.wait_terminal(terminal,b'overview saved')
            self.assertEqual(len(list((self.root/'collections').glob('formula-*/manifest.json'))),1)
            os.write(terminal,b'q\x1dd');p.wait(timeout=5)
        self.assertEqual(termios.tcgetattr(slave),original)

    def test_scrollback_wheel_anchors_output_and_keeps_capture_shared(self):
        s=self.start('--baud','115200')
        p,terminal,slave,original=self.terminal('attach',s['id'])
        intro=self.wait_terminal(terminal,b'Ctrl-]')
        self.assertIn(b'\x1b[?1000h\x1b[?1006h',intro)
        payload=b''.join(f'boot-line-{i:03}\r\n'.encode() for i in range(80))
        os.write(self.master,payload);self.wait_terminal(terminal,b'boot-line-079')
        # Slow, split SGR mouse packet: no part may become serial input.
        os.write(terminal,b'\x1b[<6');time.sleep(.35)
        self.assertFalse(select.select([self.master],[],[],.05)[0])
        os.write(terminal,b'4;20;10M')
        self.wait_terminal(terminal,b'Scrollback')
        os.write(self.master,b'new-tail-a\r\nnew-tail-b\r\n')
        output=self.wait_terminal(terminal,b'5 lines back')
        self.assertNotIn(b'new-tail-a',output)
        a=self.agent();history=a.tool('sericon_read',session=s['id'],after=0,limit=128)
        self.assertIn('new-tail-b',''.join(e.get('text','') for e in history['events']))
        self.assertFalse(select.select([self.master],[],[],.05)[0])
        os.write(terminal,b'\x1b[F')
        self.wait_terminal(terminal,b'new-tail-b')
        # End at the live prompt remains a normal DUT key.
        os.write(terminal,b'\x1b[F');self.input_bytes(b'\x1b[F')
        os.write(terminal,b'\x1dq');self.wait_terminal(terminal,b'session stopped')
        p.wait(timeout=5);self.assertEqual(termios.tcgetattr(slave),original)
        events=[json.loads(l) for l in (Path(s['log_directory'])/'events.jsonl').read_text().splitlines()]
        self.assertEqual(b''.join(base64.b64decode(e['data_base64']) for e in events if e['kind']=='rx'),payload+b'new-tail-a\r\nnew-tail-b\r\n')
        self.assertEqual(b''.join(base64.b64decode(e['data_base64']) for e in events if e['kind']=='tx'),b'\x1b[F')

    def test_scrollback_page_typing_resize_menu_and_mouse_cleanup(self):
        s=self.start('--baud','115200','--no-log')
        p,terminal,slave,original=self.terminal('attach',s['id'])
        self.wait_terminal(terminal,b'Ctrl-]')
        os.write(self.master,b''.join(f'row-{i:03}\r\n'.encode() for i in range(80)))
        self.wait_terminal(terminal,b'row-079')
        os.write(terminal,b'\x1b[5~');self.wait_terminal(terminal,b'Scrollback')
        os.write(terminal,b'\x1dm');self.wait_terminal(terminal,b'sericon  /  Menu')
        os.write(terminal,b'q');self.wait_terminal(terminal,b'Scrollback')
        fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',28,120,0,0))
        self.wait_terminal(terminal,b'Scrollback')
        os.write(terminal,b'\x1b[6~');self.wait_terminal(terminal,b'Live')
        # Legacy X10 wheel input also scrolls, without becoming an arrow key.
        os.write(terminal,b'\x1b[M'+bytes([96,52,42]));self.wait_terminal(terminal,b'Scrollback')
        os.write(terminal,b'version\r');self.input_bytes(b'version\r')
        self.wait_terminal(terminal,b'Live')
        os.write(terminal,b'\x1b[5~');self.wait_terminal(terminal,b'Scrollback')
        os.write(terminal,b'\x1db');self.wait_terminal(terminal,b'Live')
        p.send_signal(signal.SIGTERM)
        restored=self.wait_terminal(terminal,b'detached;')
        self.assertIn(b'\x1b[?1000l\x1b[?1006l',restored)
        p.wait(timeout=5);self.assertEqual(termios.tcgetattr(slave),original)
        self.assertEqual(b''.join(base64.b64decode(e['data_base64']) for e in self.read(s['id'])['events'] if e['kind']=='tx'),b'version\r')

    def test_scrollback_mouse_reports_stay_out_of_menu_prompts(self):
        s=self.start('--baud','115200')
        p,terminal,slave,original=self.terminal('attach',s['id'])
        self.wait_terminal(terminal,b'Ctrl-]')
        os.write(terminal,b'\x1df');self.wait_terminal(terminal,b'Type a literal search string')
        os.write(terminal,b'\x1b[<64;20;10M\x1b[<0;20;10M\x1b[<0;20;10m')
        os.write(terminal,b'\x1db\x1dlh');self.wait_terminal(terminal,b'Helper path:')
        os.write(terminal,b'\x1b[<65;20;10M\x1b[M'+bytes([96,52,42]))
        os.write(terminal,b'\x03qq')
        self.wait_terminal(terminal,b'/  Live')
        self.assertFalse(any(e['kind']=='tx' for e in self.read(s['id'])['events']))
        os.write(terminal,b'\x1dd');p.wait(timeout=5)
        self.assertEqual(termios.tcgetattr(slave),original)

    def test_scrollback_config_limits_and_mouse_opt_out(self):
        config=self.root/'terminal.toml'
        config.write_text('[terminal]\nscrollback_lines=4\nmouse=false\n')
        s=self.start('--baud','115200','--config',config)
        p,terminal,slave,original=self.terminal('--config',config,'attach',s['id'])
        output=self.wait_terminal(terminal,b'Ctrl-]')
        self.assertNotIn(b'\x1b[?1000h',output)
        os.write(self.master,b''.join(f'line-{i:03}\r\n'.encode() for i in range(50)))
        self.wait_terminal(terminal,b'line-049')
        os.write(terminal,b'\x1b[5~');self.wait_terminal(terminal,b'4 lines back')
        os.write(terminal,b'\x1dq');p.wait(timeout=5)
        self.assertEqual(termios.tcgetattr(slave),original)

if __name__ == "__main__":
    unittest.main(verbosity=2)
