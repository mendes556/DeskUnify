#!/usr/bin/env python3
# DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
"""Exercise real CLI commands, mDNS and DTLS with isolated dummy backends.

No native input, system clipboard or user configuration is touched.
"""
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import sys
import tempfile
import time

binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/lan-mouse").resolve()
assert sys.platform != "win32", "Two-daemon isolation requires Unix IPC sockets."


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind(("0.0.0.0", 0))
        return sock.getsockname()[1]


class Daemon:
    def __init__(self, root, name):
        self.root = root / name
        self.root.mkdir()
        self.config = self.root / "config.toml"
        self.port = free_port()
        self.config.write_text(f'port = {self.port}\nclipboard = false\ndiscovery = true\ncapture_backend = "dummy"\nemulation_backend = "dummy"\n')
        self.env = {**os.environ, "LAN_MOUSE_IPC_SOCKET": str(self.root / "ipc.sock"), "LAN_MOUSE_DUMMY_TEST_EVENTS": "1", "LAN_MOUSE_LOG_LEVEL": "info"}
        self.logpath = self.root / "daemon.log"
        self.log = self.logpath.open("a")
        self.start()

    def start(self):
        self.process = subprocess.Popen([str(binary), "--config", str(self.config), "--cert-path", str(self.root / "cert.pem"), "daemon"], env=self.env, stdout=self.log, stderr=self.log)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            result = self.command("status", check=False)
            if result.returncode == 0:
                self.snapshot = json.loads(result.stdout)
                self.fingerprint = self.snapshot["fingerprint"]
                return
            assert self.process.poll() is None, self.logpath.read_text()
            time.sleep(.05)
        raise RuntimeError("daemon did not start")

    def command(self, *args, check=True):
        result = subprocess.run([str(binary), "cli", "--json", "--timeout", "5", *map(str, args)], env=self.env, capture_output=True, text=True, timeout=40)
        if check:
            assert result.returncode == 0, f"CLI {args}: {result.stderr}"
            return json.loads(result.stdout)
        return result

    def stop(self):
        if self.process.poll() is None:
            self.command("shutdown")
            assert self.process.wait(timeout=5) == 0

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            self.process.wait(timeout=5)
        self.log.close()


def wait_for(predicate, seconds=8):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(.02)
    raise AssertionError("condition not reached before timeout")


with tempfile.TemporaryDirectory(prefix="lb-cli-", dir="/tmp") as directory:
    root = Path(directory)
    daemons = []
    try:
        a = Daemon(root, "a")
        daemons.append(a)
        b = Daemon(root, "b")
        daemons.append(b)
        assert a.snapshot["protocol_version"] == 6
        assert a.command("fingerprint") == a.fingerprint
        assert a.command("doctor", "--local", check=False).returncode != 0, "dummy incorrectly counted as native-ready"
        assert a.command("add-client", "--ips", "127.0.0.1", "--fingerprint", "invalid", check=False).returncode != 0
        assert not a.command("list")
        a.command("pause")
        assert a.command("clipboard", "true")["clipboard"]
        assert a.command("settings", "--clipboard", "false")["clipboard"] is False
        a.command("resume")
        stub = a.command("add-client", "--ips", "127.0.0.1", "--port", "9", "--position", "top")["clients"][0][0]
        a.command("deactivate", stub)
        assert not a.command("set-position", stub, "bottom")["clients"][0][2]["active"]
        a.command("set-host", stub, "localhost")
        a.command("set-ips", stub, "127.0.0.1,127.0.0.2")
        a.command("set-port", stub, "10")
        a.command("set-hooks", stub, "--enter-hook", "true", "--leave-hook", "true")
        client = a.command("list")[0]
        assert client[1]["hostname"] == "localhost" and len(client[1]["fix_ips"]) == 2
        assert client[1]["port"] == 10 and client[1]["cmd"] == "true" and client[1]["leave_cmd"] == "true"
        a.command("set-host", stub)
        assert a.command("set-ips", stub, check=False).returncode != 0  # Final address cannot be cleared.
        assert len(a.command("list")[0][1]["fix_ips"]) == 2
        a.command("set-hooks", stub)
        assert a.command("list")[0][1]["cmd"] is None
        a.command("save-config")
        a.stop()
        a.start()
        assert a.command("list")[0][1]["port"] == 10
        a.command("remove-client", a.command("list")[0][0])
        assert not a.command("list") and "[[clients]]" not in a.config.read_text()
        print("PASS: confirmed CLI configuration, validation, hooks, persistence and restart", flush=True)

        def discover():
            return any(p["fingerprint"] == b.fingerprint for p in a.command("scan", "--wait", "2"))
        wait_for(discover, 15)
        assert all(p["fingerprint"] != a.fingerprint for p in a.command("scan", "--wait", "1"))
        b.command("authorize-key", "source-a", a.fingerprint)
        paired = a.command("pair", "--fingerprint", b.fingerprint, "--position", "left", "--wait", "2")
        peer = paired["clients"][0][0]
        assert b.fingerprint in paired["authorized"]
        assert a.command("pair", "--fingerprint", b.fingerprint, "--wait", "1", check=False).returncode != 0
        a.command("deactivate", peer)
        entered, left = root / "entered", root / "left"
        a.command("set-hooks", peer, "--enter-hook", f"printf entered >> {shlex.quote(str(entered))}", "--leave-hook", f"printf left >> {shlex.quote(str(left))}")
        a.command("activate", peer)
        expected = ["motion(", "key(KeyLeftCtrl, 1)", "key(KeyLeftCtrl, 0)", "key(KeyA, 1)", "key(KeyA, 0)", "button(left, 1)", "button(left, 0)", "scroll(0, -1)", "scroll-120 (1, 120)"]
        wait_for(lambda: all(e in b.logpath.read_text() for e in expected))
        wait_for(entered.exists)
        print("PASS: real mDNS scan, fingerprint pairing and DTLS keyboard/motion/button/scroll delivery", flush=True)

        def ctrl_held():
            log = b.logpath.read_text()
            return log.count("key(KeyLeftCtrl, 1)") > log.count("key(KeyLeftCtrl, 0)")
        wait_for(ctrl_held)
        assert a.command("pause")["paused"]
        wait_for(lambda: not ctrl_held())
        wait_for(left.exists)
        before = b.logpath.read_text().count("key(KeyA, 1)")
        time.sleep(.5)
        assert b.logpath.read_text().count("key(KeyA, 1)") == before
        assert not a.command("resume")["paused"]
        wait_for(lambda: b.logpath.read_text().count("key(KeyA, 1)") > before)
        a.command("deactivate", peer)
        a.command("release")
        print("PASS: hooks, pause releases held modifier, input stops and resume reconnects", flush=True)

        reverse = b.command("add-client", "--ips", "127.0.0.1", "--port", a.port, "--position", "left", "--fingerprint", a.fingerprint)["clients"][0][0]
        wait_for(lambda: all(e in a.logpath.read_text() for e in expected))
        b.command("deactivate", reverse)
        a.command("remove-authorized-key", b.fingerprint)
        assert b.fingerprint not in a.command("authorized")
        print("PASS: reverse keyboard/mouse/scroll delivery and persisted revocation", flush=True)

        replacement = free_port()
        a.command("settings", "--port", replacement)
        wait_for(lambda: any(p["fingerprint"] == a.fingerprint and p["port"] == replacement for p in b.command("scan", "--wait", "1")), 12)
        a.stop()
        a.start()
        restored = a.command("status")
        assert restored["port"] == replacement and not restored["authorized"] and len(restored["clients"]) == 1
        a.stop()
        b.stop()
        print("PASS: discovery port update, saved identity/configuration and graceful shutdown", flush=True)

        # Standalone diagnostics honor flags and terminate after their deadline.
        for args in [("--capture-backend", "dummy", "test-capture", "--seconds", "1"), ("--emulation-backend", "dummy", "test-emulation", "--keyboard", "--mouse", "--scroll", "--seconds", "1")]:
            result = subprocess.run([str(binary), *args], env=a.env, capture_output=True, text=True, timeout=5)
            assert result.returncode == 0, result.stderr
        print("PASS: bounded capture/emulation CLI diagnostics", flush=True)
    except Exception:
        for daemon in daemons:
            print(f"\n{daemon.root.name} log:\n{daemon.logpath.read_text()[-15000:]}", file=sys.stderr)
        raise
    finally:
        for daemon in reversed(daemons):
            daemon.close()
