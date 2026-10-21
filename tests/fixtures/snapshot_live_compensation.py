#!/usr/bin/env python3
"""Real CLI/supervisor coverage with finite synthetic Gateways and no native service."""

import http.server
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import time


def append(path, row):
    # A single append keeps concurrent Gateway observations individually readable.
    with path.open("ab", buffering=0) as stream:
        stream.write((json.dumps(dict(row, time=time.monotonic())) + "\n").encode())


def process_identity(pid):
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return None if fields[0] == "Z" else fields[19]
    except FileNotFoundError:
        return None


def alive(row):
    return process_identity(row["pid"]) == row["started_at"]


def manager(case, args):
    if args[:2] == ["--user", "show"]:
        unit = Path(os.environ["HOME"]) / ".config/systemd/user" / (args[2] + ".service")
        pidfile = case / "daemon.pid"
        if unit.is_file() and pidfile.is_file():
            print("LoadState=loaded\nUnitFileState=enabled\nActiveState=active"
                  "\nSubState=running\nMainPID=" + pidfile.read_text().strip()
                  + "\nFragmentPath=" + str(unit))
            return 0
        print("LoadState=not-found\nUnitFileState=not-found\nActiveState=inactive"
              "\nSubState=dead\nMainPID=0\nFragmentPath=")
        return 4
    # Installation writes the real definition/desired plan; this shim never
    # contacts systemd. The scenario owns the real foreground daemon instead.
    return 0


def peer(case, name, args):
    if args[:2] != ["gateway", "run"]:
        return 1  # This finite synthetic Gateway has no restart-handoff protocol.
    port = int(args[args.index("--port") + 1])
    root = Path(os.environ["OCM_HOME"]) / "envs" / name
    marker = root / ".openclaw/workspace/generation.txt"
    generation = marker.read_text().strip()
    modefile = case / "mode"
    mode = modefile.read_text().strip() if modefile.exists() else "healthy"
    if name == "target" and generation == "pre-restore" and mode != "healthy":
        # Recovery can publish its child PID before this peer executes. Keep
        # that startup gap observable even on otherwise idle test workers.
        time.sleep(1)
    events = case / "events.jsonl"
    base = {"name": name, "pid": os.getpid(), "started_at": process_identity(os.getpid()),
            "generation": generation, "root_inode": root.stat().st_ino}
    append(events, dict(base, event="start"))
    rejected = name == "target" and generation == "snapshot" and mode != "healthy"
    if rejected and mode == "exit-before-ready":
        append(events, dict(base, event="exit-before-ready"))
        return 1

    frozen = False

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            nonlocal frozen
            if rejected and mode == "stop-timeout" and not frozen:
                # Runtime publication has already been observed by the readiness
                # caller. Freeze only this fixture's owned daemon so its desired
                # stop cannot be acknowledged; leave the rejected Gateway live.
                daemon = json.loads((case / "daemon-identity.json").read_text())
                assert alive(daemon), daemon
                os.kill(daemon["pid"], signal.SIGSTOP)
                frozen = True
                append(events, dict(base, event="daemon-frozen"))
            self.send_response(503 if rejected else 200)
            self.end_headers()
            self.wfile.write(json.dumps(base).encode())

        def log_message(self, *args):
            pass

    class Server(http.server.HTTPServer):
        allow_reuse_address = True

        def get_request(self):
            connection, address = super().get_request()
            connection.settimeout(1)
            return connection, address

    stop_at = None

    def stop(*args):
        nonlocal stop_at
        if stop_at is None:
            # Continue accessing the configured root during graceful shutdown:
            # merely sending SIGTERM must not authorize filesystem compensation.
            stop_at = time.monotonic() + 0.2

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    server = Server(("127.0.0.1", port), Handler)
    server.timeout = 0.01
    append(events, dict(base, event="listening", healthy=not rejected))
    deadline = time.monotonic() + 90
    crossed = False
    while time.monotonic() < deadline and (stop_at is None or time.monotonic() < stop_at):
        server.handle_request()
        try:
            current = marker.read_text().strip()
            if current != generation and not crossed:
                row = dict(base, event="crossed-root-replacement", disk_generation=current)
                (marker.parent / "stale-writer.json").write_text(json.dumps(row))
                append(events, row)
                crossed = True
        except FileNotFoundError:
            pass  # The rename window itself is not a fixture-created failure.
    server.server_close()
    append(events, dict(base, event="stopped", disk_generation=marker.read_text().strip(),
                        stopped_root_inode=root.stat().st_ino))
    return 0


def wait_until(predicate, description, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.025)
    raise AssertionError("timed out waiting for " + description)


def scenario(ocm, case, mode):
    script = Path(__file__).resolve()
    cwd = case / "workspace"
    cwd.mkdir()
    home = Path(os.environ["OCM_HOME"])
    systemctl = case / "systemctl"
    systemctl.write_text("#!/bin/sh\nexec " + shlex.join(
        [sys.executable, str(script), "manager", str(case)]) + ' "$@"\n')
    systemctl.chmod(0o700)
    env = dict(os.environ, OCM_INTERNAL_SERVICE_MANAGER="systemd-user",
               OCM_INTERNAL_SYSTEMCTL_BIN=str(systemctl),
               OCM_INTERNAL_JOURNALCTL_BIN="/bin/true",
               OCM_INTERNAL_GATEWAY_READINESS_TIMEOUT_MS="2000",
               OCM_SERVICE_EXECUTABLE=str(ocm))
    assert "OCM_INTERNAL_SKIP_SERVICE_READINESS" not in env
    calls = []
    daemon = None

    def run(*args, expect=True, observe=lambda: None):
        with subprocess.Popen([str(ocm), *args], env=env, cwd=cwd,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE) as command:
            deadline = time.monotonic() + 25
            try:
                while True:
                    observe()
                    try:
                        output, error = command.communicate(timeout=0.01)
                        break
                    except subprocess.TimeoutExpired:
                        assert time.monotonic() < deadline, ("CLI timeout", args)
            finally:
                if command.poll() is None:
                    command.kill()
                    command.communicate()
        result = subprocess.CompletedProcess(args, command.returncode, output.decode(), error.decode())
        calls.append({"args": args, "code": result.returncode,
                      "stdout": result.stdout, "stderr": result.stderr})
        if expect:
            assert result.returncode == 0, calls[-1]
        return result

    def events():
        path = case / "events.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def runtime():
        try:
            return json.loads((home / "supervisor/runtime.json").read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            return {}

    def child(name):
        return next((row for row in runtime().get("children", []) if row["envName"] == name), None)

    try:
        for name in ["sibling", "target"]:
            launcher_cwd = cwd / name
            launcher_cwd.mkdir()
            command = shlex.join(["exec", sys.executable, str(script), "peer", str(case), name])
            run("launcher", "add", name, "--command", command, "--cwd", str(launcher_cwd))
            # Reuse the complete-family allocator and registered-peer exclusion.
            run("env", "create", name, "--launcher", name)
            marker = home / "envs" / name / ".openclaw/workspace/generation.txt"
            marker.parent.mkdir(parents=True, exist_ok=True)
            marker.write_text("snapshot" if name == "target" else "sibling")
        run("service", "install", "sibling", "--json")
        with (case / "daemon.log").open("w") as log:
            daemon = subprocess.Popen([str(ocm), "__daemon", "run"], cwd=cwd, env=env,
                                      stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                      start_new_session=True)
        (case / "daemon.pid").write_text(str(daemon.pid))
        (case / "daemon-identity.json").write_text(json.dumps(
            {"pid": daemon.pid, "started_at": process_identity(daemon.pid)}))
        wait_until(lambda: runtime().get("gatewayAdmission"), "daemon admission")
        run("service", "start", "sibling", "--json")
        run("service", "start", "target", "--json")
        snapshot = json.loads(run("env", "snapshot", "create", "target", "--json").stdout)
        assert snapshot["serviceEnabled"] and snapshot["serviceRunning"], snapshot
        run("service", "stop", "target")
        wait_until(lambda: not child("target"), "target stop")
        marker = home / "envs/target/.openclaw/workspace/generation.txt"
        marker.write_text("pre-restore")
        run("service", "start", "target", "--json")
        before = child("target")
        sibling_before = child("sibling")["pid"]
        (case / "mode").write_text(mode)
        offset = len(events())
        live_at_replacement = []

        def observe_replacement():
            restored = [row for row in events()[offset:] if row["event"] == "start"
                        and row["name"] == "target" and row["generation"] == "snapshot"]
            try:
                if marker.read_text().strip() == "pre-restore":
                    live_at_replacement.extend(row for row in restored if alive(row))
            except FileNotFoundError:
                pass

        restored = run("env", "snapshot", "restore", "target", snapshot["id"], "--json",
                       expect=mode == "healthy", observe=observe_replacement)

        def started_target():
            observe_replacement()
            current = child("target")
            if current and any(row["event"] == "listening" and row["name"] == "target"
                               and row["pid"] == current["pid"] for row in events()[offset:]):
                return current
            return None

        # Supervisor admission can precede the peer's own startup observations.
        # Keep observing replacement until that specific process is listening,
        # then freeze the event snapshot used by the generation assertions.
        after = None if mode == "stop-timeout" else wait_until(
            started_target, "restored or recovered target startup")
        observe_replacement()
        observed = events()[offset:]
        starts = [row for row in observed if row["event"] == "start" and row["name"] == "target"]
        snapshot_starts = [row for row in starts if row["generation"] == "snapshot"]
        assert snapshot_starts, observed
        assert child("sibling")["pid"] == sibling_before, runtime()
        assert not [row for row in observed if row["name"] == "sibling"], observed
        assert not live_at_replacement, live_at_replacement
        assert not [row for row in events() if row["event"] == "crossed-root-replacement"], events()
        assert not (marker.parent / "stale-writer.json").exists()
        operations = list((home / "envs").glob(".target-ocm-restore-*"))
        if mode == "stop-timeout":
            assert restored.returncode != 0 and "quiescence" in restored.stderr, calls[-1]
            assert marker.read_text() == "snapshot"
            assert marker.parents[2].stat().st_ino == snapshot_starts[0]["root_inode"]
            assert all(alive(row) for row in snapshot_starts), snapshot_starts
            assert any(row["event"] == "daemon-frozen" for row in observed), observed
            assert not [row for row in starts if row["generation"] == "pre-restore"], starts
            assert len(operations) == 1, operations
            assert str(operations[0]) in restored.stderr, restored.stderr
            assert (operations[0] / "displaced/.openclaw/workspace/generation.txt").read_text() == "pre-restore"
            registry = json.loads((home / "envs.json").read_text())
            target = next(row for row in registry["envs"] if row["name"] == "target")
            assert not target["serviceRunning"], target
        else:
            assert not operations, operations
            assert after and after["pid"] != before["pid"], runtime()
            expected = "snapshot" if mode == "healthy" else "pre-restore"
            assert marker.read_text() == expected
            accepted = next(row for row in starts if row["pid"] == after["pid"])
            assert accepted["generation"] == expected and alive(accepted), accepted
            if mode != "healthy":
                assert restored.returncode != 0 and "failed managed-service acceptance" in restored.stderr, calls[-1]
                assert all(not alive(row) and row["pid"] != after["pid"] for row in snapshot_starts)
                if mode == "unhealthy-live":
                    for rejected in snapshot_starts:
                        stopped = next(row for row in observed
                                       if row["event"] == "stopped" and row["pid"] == rejected["pid"])
                        assert stopped["disk_generation"] == "snapshot", stopped
                        assert stopped["stopped_root_inode"] == rejected["root_inode"], stopped
                        assert stopped["time"] < accepted["time"], (stopped, accepted)
                else:
                    assert any(row["event"] == "exit-before-ready" for row in observed), observed
        print(json.dumps({"mode": mode, "events": observed, "restore_error": restored.stderr}))
    except BaseException:
        print(json.dumps({"mode": mode, "calls": calls, "events": events(), "runtime": runtime(),
                          "daemon_log": (case / "daemon.log").read_text()
                          if (case / "daemon.log").exists() else ""}), flush=True)
        raise
    finally:
        if daemon is not None and daemon.poll() is None:
            daemon.send_signal(signal.SIGCONT)
            daemon.terminate()
            try:
                daemon.wait(timeout=12)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait(timeout=3)
        peers = [row for row in events() if row["event"] == "start"]
        for row in peers:
            if alive(row):
                try:
                    os.kill(row["pid"], signal.SIGKILL)
                except ProcessLookupError:
                    pass
        wait_until(lambda: not any(alive(row) for row in peers), "owned Gateway cleanup")


if __name__ == "__main__":
    if sys.argv[1] == "manager":
        sys.exit(manager(Path(sys.argv[2]), sys.argv[3:]))
    if sys.argv[1] == "peer":
        sys.exit(peer(Path(sys.argv[2]), sys.argv[3], sys.argv[4:]))
    scenario(Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3])
