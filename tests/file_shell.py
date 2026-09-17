"""An isolated shell at the far end of a serial PTY, with output corruption."""
import os
import re
import select
import subprocess
import threading
import time


class Shell:
    def __init__(self, fd, env=None, corrupt=None, before=None):
        self.fd, self.env, self.corrupt, self.before = fd, env, corrupt, before
        self.stop = threading.Event()
        self.commands = []
        self.errors = []
        self.thread = threading.Thread(target=self.loop, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *args):
        self.stop.set()
        self.thread.join(5)
        assert not self.thread.is_alive(), "shell fixture did not stop"
        if self.errors:
            raise self.errors[0]

    def loop(self):
        pending = b""
        command = b""
        try:
            while not self.stop.is_set():
                if not select.select([self.fd], [], [], .02)[0]:
                    continue
                pending += os.read(self.fd, 8192)
                while b"\r" in pending:
                    line, pending = pending.split(b"\r", 1)
                    assert len(line) < 128, "command exceeds small DUT line editor limit"
                    command += line + b"\n"
                    if line.endswith(b"\\"):
                        continue
                    text = command.decode()
                    normalized = text.replace("\\\n", "")
                    self.commands.append(normalized)
                    command = b""
                    if self.before:
                        self.before(normalized, self)
                    if self.stop.is_set():
                        return
                    result = subprocess.run(["/bin/sh", "-c", text], stdin=subprocess.DEVNULL,
                                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                            env=self.env, timeout=3)
                    output = result.stdout.replace(b"\n", b"\r\n")
                    if self.corrupt:
                        output = self.corrupt(normalized, output)
                    # Echoed commands, boot-like noise, and split UART events
                    # are deliberately present even for the clean fixture.
                    reply = text.encode().replace(b"\n", b"\r\n") + b"[console] background task\r\n" + output + b"~ # "
                    for at in range(0, len(reply), 173):
                        os.write(self.fd, reply[at:at+173])
        except Exception as exc:
            self.errors.append(exc)


def corrupt_chunk(output, mode="alphabet"):
    match = re.search(rb"SC[0-9a-f]+:B\r\n", output)
    assert match
    at = match.end() + 12
    if mode == "alphabet":
        # Same length and valid base64 alphabet, so decoder/length alone pass.
        return output[:at] + (b"B" if output[at:at+1] == b"A" else b"A") + output[at+1:]
    return output[:at] + b"kernel: link down\r\n" + output[at:]
