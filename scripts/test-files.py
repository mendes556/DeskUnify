#!/usr/bin/env python3
# DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
"""Real CLI/TLS file-transfer acceptance. Isolated identities; no input/OS clipboard."""
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time

binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/lan-mouse").resolve()

def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]

def final(result):
    assert result.returncode == 0, result.stderr
    return [json.loads(line) for line in result.stdout.splitlines()][-1]

with tempfile.TemporaryDirectory(prefix="lan-bridge-files-") as directory:
    root = Path(directory)
    for side in ["a", "b"]:
        (root / side).mkdir()
        (root / side / "config.toml").write_text("discovery = false\nclipboard = false\n")

    def command(side, *args, **kwargs):
        return [str(binary), "--config", str(root / side / "config.toml"), "--cert-path", str(root / side / "cert.pem"), "files", "--json", *map(str, args)]

    identities = {side: final(subprocess.run(command(side, "identity"), capture_output=True, text=True, timeout=20))["fingerprint"] for side in ["a", "b"]}
    for side, other in [("a", "b"), ("b", "a")]:
        (root / side / "config.toml").write_text("discovery = false\nclipboard = false\n[authorized_fingerprints]\n" + json.dumps(identities[other]) + ' = "test"\n')
    address = f"127.0.0.1:{port()}"
    output = root / "received"
    log = (root / "receiver.log").open("w+")
    receiver = subprocess.Popen(command("b", "receive", "--output", output, "--listen", address, "--allow-benchmark"), stdout=log, stderr=log)
    try:
        for _ in range(200):
            log.flush()
            if '"event":"listening"' in (root / "receiver.log").read_text(): break
            assert receiver.poll() is None, (root / "receiver.log").read_text()
            time.sleep(.025)
        else: raise RuntimeError("receiver did not start")
        def send(*args, success=True):
            result = subprocess.run(command("a", *args), capture_output=True, text=True, timeout=90)
            if success: return final(result)
            assert result.returncode != 0
            return result
        bench = send("benchmark", "--to", address, "--mode", "full", "--size-mib", "8")
        assert bench["bytes"] == 8 * 1048576 and bench["output"] is None
        assert not list(output.iterdir()), "benchmark wrote files"
        send("benchmark", "--to", address, "--fingerprint", ":".join(["00"] * 32), "--size-mib", "1", success=False)
        source = root / "fixtures"
        (source / "empty").mkdir(parents=True)
        data = source / "large.bin"
        with data.open("wb") as stream:
            for _ in range(32): stream.write(os.urandom(1048576))
        (source / "中文 #%.txt").write_text("中文 English 😀\n" * 10)
        (source / "empty-file").touch()
        for index in range(200): (source / f"small-{index:04d}").write_bytes(bytes([index % 256]) * 1024)
        args = ["send", "--to", address, "--mode", "full", "--limit-mib", "2", source]
        sender = subprocess.Popen(command("a", *args), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        while True:
            line = sender.stdout.readline()
            assert line, sender.stderr.read()
            event = json.loads(line)
            if event.get("event") == "progress" and event["network_bytes"] >= 1048576: break
        # Sender progress can precede the receiver's TLS read/disk write. Wait
        # for complete chunks on the receiving side before interrupting.
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            partial = list(output.glob(".lan-bridge-*.partial/fixtures/large.bin"))
            if partial and partial[0].stat().st_size >= 2 * 1048576:
                break
            assert sender.poll() is None, "sender exited before interruption"
            time.sleep(.025)
        else:
            raise AssertionError("receiver did not persist complete chunks before interruption")
        sender.send_signal(signal.SIGINT)
        sender.communicate(timeout=5)
        assert sender.returncode != 0
        resumed = send("send", "--to", address, "--mode", "full", "--fingerprint", identities["b"], source)
        assert resumed["resumed_bytes"] >= 1048576
        received = Path(resumed["output"]) / source.name
        for path in source.rglob("*"):
            target = received / path.relative_to(source)
            if path.is_dir(): assert target.is_dir()
            else: assert hashlib.sha256(path.read_bytes()).digest() == hashlib.sha256(target.read_bytes()).digest(), path
        repeat = send("send", "--to", address, "--mode", "auto", source)
        assert repeat["network_bytes"] == 0 and repeat["resumed_bytes"] == repeat["bytes"]
        # Source links must never be dereferenced and sent.
        if hasattr(os, "symlink"):
            link = root / "link"
            link.symlink_to(data)
            send("send", "--to", address, link, success=False)
        print(json.dumps({"result":"passed", "files":resumed["files"], "resumed_bytes":resumed["resumed_bytes"], "network_mib_s":bench["mib_per_second"], "file_mib_s":resumed["mib_per_second"]}))
    finally:
        receiver.send_signal(signal.SIGINT)
        receiver.wait(timeout=5)
        log.close()
