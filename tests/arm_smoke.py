#!/usr/bin/env python3
"""Run the static ARM brokers under QEMU with PTYs, never physical adapters."""
import base64
import json
import os
from pathlib import Path
import pty
import select
import socket
import subprocess
import tempfile
import time
from file_shell import Shell

ROOT = Path(__file__).resolve().parents[1]


def check(target, emulator):
    with tempfile.TemporaryDirectory(prefix="sericon-arm-fixture-") as folder:
        master, slave = pty.openpty()
        env = dict(os.environ, SERICON_RUNTIME_DIR=folder + "/runtime")
        spec = {
            "config": {"serial": {"baud": 115200}, "logging": {"enabled": False}},
            "device": {"port": os.ttyname(slave)},
            "launch_dir": folder,
        }
        process = subprocess.Popen(
            [*emulator, str(ROOT / "target" / target / "release/sericon"), "__serve"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, env=env,
        )
        try:
            process.stdin.write(json.dumps(spec) + "\n")
            process.stdin.flush()
            assert select.select([process.stdout], [], [], 10)[0], target
            response = json.loads(process.stdout.readline())
            assert "result" in response, response
            session = response["result"]["id"]
            path = folder + "/runtime/" + session + ".sock"

            def call(request):
                with socket.socket(socket.AF_UNIX) as stream:
                    stream.settimeout(5)
                    stream.connect(path)
                    stream.sendall(json.dumps(request).encode() + b"\n")
                    with stream.makefile("rb") as incoming:
                        return json.loads(incoming.readline())

            os.write(master, b"Emulated Pi UART input\r\n")
            page = call({"op": "read", "after": 1, "wait_ms": 2000, "limit": 32})["result"]
            assert any("Emulated Pi UART" in e.get("text", "") for e in page["events"]), page
            reply = call({"op": "send", "actor": "fixture", "client_id": "fixture", "data_base64": base64.b64encode(b"help\r").decode(), "release": True})
            assert reply["result"]["written"] == 5, reply
            assert select.select([master], [], [], 2)[0]
            assert os.read(master, 100) == b"help\r"
            run = call({"op": "formula_start", "formula": {"name": "arm-fixture", "source": 'emit(#{value:40 + 2});'},
                        "initiator": "fixture", "params": {}})["result"]["id"]
            deadline = time.monotonic() + 10
            while True:
                result = call({"op": "formula_read", "run": run})["result"]
                if result["run"]["state"] != "running":
                    break
                assert time.monotonic() < deadline, result
                time.sleep(0.05)
            assert result["run"]["state"] == "completed", result
            assert result["results"] == [{"value": 42}], result
            dut = Path(folder) / "dut.bin"
            dut.write_bytes(bytes(range(256)) * 5)
            with Shell(master):
                run = call({"op":"formula_start", "formula":{"name":"arm-files", "interactive":True,
                            "source":'emit(linux_download(params.path, "dump.bin"));',
                            "parameters":{"path":{"type":"string"}}}, "params":{"path":str(dut)},
                            "output_dir":folder + "/downloads", "initiator":"fixture"})["result"]["id"]
                deadline = time.monotonic() + 20
                while True:
                    result = call({"op":"formula_read", "run":run})["result"]
                    if result["run"]["state"] != "running":
                        break
                    assert time.monotonic() < deadline, result
                    time.sleep(.05)
                assert result["run"]["state"] == "completed", result
                assert Path(result["run"]["artifacts"][0]["path"]).read_bytes() == dut.read_bytes()
                helper = ROOT / "target/helpers/x86_64"
                if helper.exists():
                    uploaded = Path(folder) / "uploaded.bin"
                    run = call({"op":"formula_start", "formula":{"name":"arm-helper", "interactive":True,
                                "source":'emit(linux_upload(params.source, params.dest, params.helper, false)); emit(linux_collect_overview(params.helper));',
                                "parameters":{k:{"type":"string"} for k in ("source","dest","helper")}},
                                "params":{"source":str(dut),"dest":str(uploaded),"helper":str(helper)},
                                "output_dir":folder+"/overview", "initiator":"fixture"})["result"]["id"]
                    deadline=time.monotonic()+20
                    while True:
                        result=call({"op":"formula_read","run":run})["result"]
                        if result["run"]["state"]!="running": break
                        assert time.monotonic()<deadline,result
                        time.sleep(.05)
                    assert result["run"]["state"]=="completed",result
                    assert uploaded.read_bytes()==dut.read_bytes()
                    assert len(result["run"]["artifacts"])==2,result
            call({"op": "stop"})
            assert process.wait(timeout=5) == 0
            print(target + ": emulated broker, PTY RX/TX, Rhai, download, helper upload/overview collection, shared API, and clean stop passed")
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
            process.stdin.close()
            process.stdout.close()
            process.stderr.close()
            os.close(master)
            os.close(slave)


if __name__ == "__main__":
    check("aarch64-unknown-linux-musl", ["qemu-aarch64"])
    check("arm-unknown-linux-musleabihf", ["qemu-arm", "-cpu", "arm1176"])
