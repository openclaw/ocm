#!/usr/bin/env python3
"""Published OpenClaw lifecycle proof on a fresh disposable macOS user."""

import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import plistlib
import pwd
import re
import shutil
import socket
import subprocess
import sys
import tempfile
from urllib.parse import urlsplit

from native_gateway_provider import ProviderFixture

LABEL = "ai.openclaw.ocm"
ENV_NAME = "native-gateway"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--preflight-only", action="store_true")
    parser.add_argument("--ocm", type=Path)
    parser.add_argument("--openclaw-version")
    parser.add_argument("--disposable-host", action="store_true")
    args = parser.parse_args()
    if not args.preflight_only and not (args.ocm and args.openclaw_version and args.disposable_host):
        parser.error("--ocm, --openclaw-version, and --disposable-host are required")
    if sys.platform != "darwin" or os.getuid() == 0:
        parser.error("this check requires a native macOS session as a disposable non-root user")
    home = Path(os.environ["HOME"]).resolve(strict=True)
    if home != Path(pwd.getpwuid(os.getuid()).pw_dir).resolve(strict=True):
        parser.error("HOME must belong to the current macOS user")
    env = {key: value for key, value in os.environ.items() if key in {
        "HOME", "PATH", "USER", "LOGNAME", "LANG", "LC_ALL", "TMPDIR",
    }}
    domain = "gui/" + str(os.getuid())
    target = domain + "/" + LABEL
    definition = home / "Library/LaunchAgents" / (LABEL + ".plist")

    def launchctl(*arguments):
        return subprocess.run(
            ["/bin/launchctl", *arguments], env=env,
            capture_output=True, text=True, timeout=30,
        )

    def job():
        result = launchctl("print", target)
        if result.returncode:
            if 'Could not find service "' + LABEL + '"' in result.stderr and "in domain" in result.stderr:
                return None
            raise RuntimeError("native OCM job state is unknown")
        return dict(re.findall(r"^\s*(path|pid) = (.+)$", result.stdout, re.MULTILINE))

    def preflight():
        for command in [("managername",), ("print", domain)]:
            if launchctl(*command).returncode:
                raise RuntimeError("a usable GUI launchd domain is required; no service was changed")
        if definition.exists() or definition.is_symlink() or job() is not None:
            raise RuntimeError("existing native OCM service; use a fresh disposable user")
        disabled = launchctl("print-disabled", domain)
        if disabled.returncode:
            raise RuntimeError("launchd enablement state is unknown")
        if re.search(r'^\s*"' + re.escape(LABEL) + r'"\s*=>', disabled.stdout, re.MULTILINE):
            raise RuntimeError("existing OCM launchd enablement override; use a fresh disposable user")

    preflight()
    if args.preflight_only:
        print("PASS: fresh native macOS GUI launchd session")
        return

    candidate = args.ocm.resolve(strict=True)
    digest = hashlib.sha256(candidate.read_bytes()).digest()
    root = Path(tempfile.mkdtemp(prefix="ocm-native-gateway-"))
    print("isolated test state:", root, flush=True)
    binary = root / "ocm"
    # A Cargo target may intentionally select another installed OCM for its daemon.
    shutil.copy2(candidate, binary)
    env["OCM_HOME"] = str(root / "store")
    env["NO_COLOR"] = "1"
    libproc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    libproc.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    libproc.proc_pidpath.restype = ctypes.c_int

    def executable(pid):
        buffer = ctypes.create_string_buffer(4096)
        if libproc.proc_pidpath(pid, buffer, len(buffer)) <= 0:
            raise RuntimeError("cannot inspect native executable for PID " + str(pid))
        return Path(os.fsdecode(buffer.value))

    def exists(pid):
        try:
            os.kill(pid, 0)
            return True
        except ProcessLookupError:
            return False

    def oc_json(label, *arguments, timeout=180):
        result = subprocess.run(
            [str(binary), *map(str, arguments), "--json"], cwd=root, env=env,
            capture_output=True, text=True, timeout=timeout,
        )
        print(label, "exit", result.returncode, flush=True)
        if result.returncode:
            log = root / (label.replace(" ", "-") + ".log")
            log.write_text(result.stdout + result.stderr)
            log.chmod(0o600)
            raise RuntimeError(label + " failed; private diagnostics retained")
        return json.loads(result.stdout)

    def owned_job():
        observed = job()
        if definition.exists() or definition.is_symlink():
            if definition.is_symlink():
                raise RuntimeError("unexpected service definition symlink; retaining state")
            plist = plistlib.loads(definition.read_bytes())
            assert plist["EnvironmentVariables"]["OCM_HOME"] == env["OCM_HOME"]
            assert Path(plist["ProgramArguments"][0]).resolve() == binary.resolve()
        if observed is not None:
            assert definition.exists() and Path(observed["path"]).resolve() == definition.resolve()
            pid = int(observed.get("pid", "0"))
            if pid:
                assert hashlib.sha256(executable(pid).read_bytes()).digest() == digest
        return observed

    pids = set()

    def healthy(label):
        assert oc_json(label, "@" + ENV_NAME, "--", "gateway", "health")["ok"] is True
        status = oc_json("service status", "service", "status", ENV_NAME)
        observed = owned_job()
        assert int(observed["pid"]) == status["ocmServicePid"]
        assert status["running"] and status["childPid"]
        pids.update((status["ocmServicePid"], status["childPid"]))
        parent = subprocess.check_output(
            ["/bin/ps", "-p", str(status["childPid"]), "-o", "ppid="], env=env, text=True, timeout=30,
        )
        assert int(parent) == status["ocmServicePid"]
        node = subprocess.check_output(
            [executable(status["childPid"]), "-p", "process.release.name"], env=env, text=True, timeout=30,
        ).strip()
        assert node == "node"
        probe = oc_json("native Gateway identity", "@" + ENV_NAME, "--", "gateway", "probe")
        assert any(
            item["connect"]["ok"]
            and urlsplit(item["url"]).hostname in {"127.0.0.1", "localhost", "::1"}
            and urlsplit(item["url"]).port == port
            and item["server"]["version"] == args.openclaw_version
            for item in probe["targets"]
        )
        return status

    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    provider = ProviderFixture()
    install_attempted = False
    failure = None
    try:
        installed = oc_json(
            "published install", "start", ENV_NAME, "--version", args.openclaw_version,
            "--port", port, "--no-service", timeout=1200,
        )
        assert installed["created"] and not installed["serviceRequested"]
        oc_json("runtime integrity", "runtime", "verify", installed["defaultRuntime"])
        inventory = oc_json("plugin inventory", "@" + ENV_NAME, "--", "plugins", "list")
        assert inventory["plugins"]
        assert not [item["id"] for item in inventory["plugins"] if item.get("status") == "error"]
        details = oc_json("provider environment", "env", "show", ENV_NAME)
        provider.configure(Path(details["configPath"]))
        # Package installation awaited network work; recheck native custody at the effect.
        preflight()
        install_attempted = True
        assert oc_json("service install", "service", "install", ENV_NAME)["installed"]
        assert oc_json("service start", "service", "start", ENV_NAME)["gatewayReady"] is True
        before = healthy("authenticated health")
        provider.check(oc_json, ENV_NAME, "before restart")
        assert oc_json("service restart", "service", "restart", ENV_NAME)["gatewayReady"] is True
        after = healthy("authenticated health after restart")
        assert after["childPid"] != before["childPid"] and not exists(before["childPid"])
        provider.check(oc_json, ENV_NAME, "after restart")
        stopped = oc_json("service stop", "service", "stop", ENV_NAME)
        assert not stopped["running"] and not stopped["desiredRunning"]
        status = oc_json("stopped status", "service", "status", ENV_NAME)
        assert status["installed"] and not status["running"] and status["childPid"] is None
        assert all(not exists(pid) for pid in pids)
        with socket.socket() as connection:
            connection.settimeout(2)
            assert connection.connect_ex(("127.0.0.1", port)) != 0
    except BaseException as error:
        failure = error
        raise
    finally:
        try:
            if install_attempted:
                try:
                    observed = owned_job()
                    if observed and observed.get("pid"):
                        pids.add(int(observed["pid"]))
                    assert not oc_json("service uninstall", "service", "uninstall", ENV_NAME)["installed"]
                    assert oc_json("uninstalled environment policy", "env", "show", ENV_NAME)["serviceEnabled"] is False
                    assert job() is None and not definition.exists() and not definition.is_symlink()
                    assert all(not exists(pid) for pid in pids)
                except BaseException as error:
                    print("native cleanup failed; retain isolated state:", root, file=sys.stderr)
                    if failure is None:
                        raise
                    print(type(error).__name__ + ": " + str(error), file=sys.stderr)
        finally:
            provider.close()
    # launchctl's task-created enabled override remains on this disposable user.
    shutil.rmtree(root)
    print("PASS: published Gateway lifecycle and native launchd cleanup", flush=True)


if __name__ == "__main__":
    main()
