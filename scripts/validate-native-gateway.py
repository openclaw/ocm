#!/usr/bin/env python3
"""Opt-in published OpenClaw lifecycle proof on a disposable Linux host."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import sys
import tempfile
from urllib.parse import urlsplit

from native_gateway_provider import ProviderFixture
from native_gateway_status import stop_gateway

UNIT = "ai.openclaw.ocm.service"
ENV_NAME = "native-gateway"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ocm", type=Path, required=True)
    parser.add_argument("--openclaw-version", required=True)
    parser.add_argument(
        "--disposable-host", action="store_true", required=True,
        help="acknowledge that this user's native OCM service will be installed",
    )
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("this check requires native Linux systemd --user")
    binary = args.ocm.resolve(strict=True)
    candidate_digest = hashlib.sha256(binary.read_bytes()).digest()
    # Keep the real user bus/home, without inheriting product overrides or credentials.
    env = {
        key: value for key, value in os.environ.items()
        if key in {
            "HOME", "PATH", "USER", "LOGNAME", "LANG", "LC_ALL",
            "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "TMPDIR",
        }
    }
    subprocess.run(
        ["systemctl", "--user", "show-environment"], env=env,
        check=True, capture_output=True, timeout=30,
    )

    def unit_property(name):
        return subprocess.check_output(
            ["systemctl", "--user", "show", UNIT, "--property=" + name, "--value"],
            env=env, text=True, timeout=30,
        ).strip()

    definition = Path.home() / ".config/systemd/user" / UNIT
    if definition.exists() or definition.is_symlink() or unit_property("LoadState") != "not-found":
        raise RuntimeError("existing native OCM service; use a fresh disposable user")

    root = Path(tempfile.mkdtemp(prefix="ocm-native-gateway-"))
    print("isolated test state:", root, flush=True)
    # Cargo artifacts may intentionally select an installed OCM for the daemon.
    # Stage this exact candidate outside target/ so the test exercises its bytes.
    staged_binary = root / "ocm"
    shutil.copy2(binary, staged_binary)
    binary = staged_binary
    env["OCM_HOME"] = str(root / "store")
    env["NO_COLOR"] = "1"

    def run(label, *arguments, timeout=180):
        result = subprocess.run(
            [str(binary), *map(str, arguments)], cwd=root, env=env,
            capture_output=True, text=True, timeout=timeout,
        )
        print(label, "exit", result.returncode, flush=True)
        if result.returncode:
            # Private diagnostics stay with the isolated state, never in public output.
            diagnostic = root / (label.replace(" ", "-") + ".log")
            diagnostic.write_text(result.stdout + result.stderr)
            diagnostic.chmod(0o600)
            raise RuntimeError(label + " failed; diagnostics retained at " + str(diagnostic))
        return result.stdout

    def oc_json(label, *arguments, **options):
        return json.loads(run(label, *arguments, "--json", **options))

    def status():
        return oc_json("service status", "service", "status", ENV_NAME)

    def healthy(label):
        health = oc_json(label, "@" + ENV_NAME, "--", "gateway", "health")
        assert health["ok"] is True
        observed = status()
        assert observed["running"] and observed["childPid"]
        assert unit_property("ActiveState") == "active"
        assert int(unit_property("MainPID")) == observed["ocmServicePid"]
        daemon = Path("/proc") / str(observed["ocmServicePid"]) / "exe"
        assert hashlib.sha256(daemon.read_bytes()).digest() == candidate_digest
        child = Path("/proc") / str(observed["childPid"])
        parent = next(line for line in (child / "status").read_text().splitlines() if line.startswith("PPid:"))
        assert int(parent.split()[1]) == observed["ocmServicePid"]
        node = subprocess.check_output(
            [child / "exe", "-p", "process.release.name"],
            env=env, text=True, timeout=30,
        ).strip()
        assert node == "node"
        probe = oc_json("native Gateway identity", "@" + ENV_NAME, "--", "gateway", "probe")
        assert any(
            target["connect"]["ok"]
            and urlsplit(target["url"]).hostname in {"127.0.0.1", "localhost", "::1"}
            and urlsplit(target["url"]).port == port
            and target["server"]["version"] == args.openclaw_version
            for target in probe["targets"]
        )
        return observed

    provider = ProviderFixture()
    created = False
    try:
        installed = oc_json(
            "published install", "start", ENV_NAME,
            "--version", args.openclaw_version, "--no-service",
            timeout=1200,
        )
        created = installed["created"]
        assert created and not installed["serviceRequested"]
        port = installed["gatewayPort"]
        runtime = installed["defaultRuntime"]
        oc_json("runtime integrity", "runtime", "verify", runtime)
        version = run("published version", "@" + ENV_NAME, "--", "--version")
        assert re.search(r"\b" + re.escape(args.openclaw_version) + r"\b", version)
        inventory = oc_json("plugin inventory", "@" + ENV_NAME, "--", "plugins", "list")
        assert inventory["plugins"], "published plugin inventory is empty"
        errors = [plugin["id"] for plugin in inventory["plugins"] if plugin.get("status") == "error"]
        assert not errors, errors
        print("native plugin discovery:", len(inventory["plugins"]), "plugins", flush=True)

        details = oc_json("provider environment", "env", "show", ENV_NAME)
        provider.configure(Path(details["configPath"]))

        assert oc_json("service install", "service", "install", ENV_NAME)["installed"]
        assert oc_json("service start", "service", "start", ENV_NAME)["gatewayReady"] is True
        before = healthy("authenticated health")
        provider.check(oc_json, ENV_NAME, "before restart")
        assert oc_json("service restart", "service", "restart", ENV_NAME)["gatewayReady"] is True
        after = healthy("authenticated health after restart")
        assert after["childPid"] != before["childPid"]
        assert not (Path("/proc") / str(before["childPid"])).exists()
        provider.check(oc_json, ENV_NAME, "after restart")
        stop_gateway(oc_json, ENV_NAME)
        final = status()
        assert final["installed"] and not final["running"] and final["childPid"] is None
        assert not (Path("/proc") / str(after["childPid"])).exists()
        assert not (Path("/proc") / str(after["ocmServicePid"])).exists()
        with socket.socket() as connection:
            connection.settimeout(2)
            assert connection.connect_ex(("127.0.0.1", port)) != 0
    finally:
        try:
            # Stop can remove the shared unit while leaving environment service policy enabled.
            if created or definition.exists() or definition.is_symlink() or unit_property("LoadState") != "not-found":
                daemon_pid = int(unit_property("MainPID"))
                removed = oc_json("service uninstall", "service", "uninstall", ENV_NAME)
                assert not removed["installed"]
                assert oc_json("uninstalled environment policy", "env", "show", ENV_NAME)["serviceEnabled"] is False
                assert unit_property("LoadState") == "not-found"
                assert not definition.exists() and not definition.is_symlink()
                assert not daemon_pid or not (Path("/proc") / str(daemon_pid)).exists()
        finally:
            provider.close()
    shutil.rmtree(root)
    print("PASS: published Gateway lifecycle and native service cleanup", flush=True)


if __name__ == "__main__":
    main()
