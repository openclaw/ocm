#!/usr/bin/env python3
"""Finite, real released-OCM upgrade proof on a disposable native Linux runner."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import socket
import shutil
import subprocess
import sys
import tarfile
import time
import traceback
import urllib.request
from urllib.parse import urlsplit

RELEASE = "v0.3.5"
OPENCLAW = "2026.10.5"
UNIT = "ai.openclaw.ocm.service"
ENV_NAME = "released-upgrade"
SESSION_KEY = "agent:main:ocm-released-upgrade"
SESSION_LABEL = "Durable state before OCM security upgrade"
ASSETS = {
    "install.sh": "edc475d2e6e257c461655f514035533975b4c7f1ffe9a74fa987423be402e072",
    "ocm-x86_64-unknown-linux-gnu.tar.gz": "0b937e1c23879c24cd22f530436e53661d729900f96eeba40e2275dd1b587a83",
    "SHA256SUMS": "dab663cc669c226baadae085aede4a2d5e0ad2b9a8a9eeb33e6c383b23665558",
}


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n")


def command(args, **kwargs):
    return subprocess.run([str(a) for a in args], check=True, capture_output=True,
                          text=True, timeout=kwargs.pop("timeout", 120), **kwargs)


def guard():
    if (platform.system(), platform.machine()) != ("Linux", "x86_64"):
        raise RuntimeError("Requires the Linux x86_64 hosted lane")
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("GITHUB_REPOSITORY") != "openclaw/ocm":
        raise RuntimeError("Requires the disposable openclaw/ocm hosted CI lane")


def actor(meta, action):
    user = meta["user"]
    uid = meta["uid"]
    env = ["HOME=" + meta["home"], "USER=" + user, "LOGNAME=" + user,
           "PATH=" + meta["ambientBin"] + ":/usr/bin:/bin", "SHELL=/bin/bash",
           "OCM_HOME=" + meta["home"] + "/store", "TMPDIR=" + meta["home"] + "/tmp",
           "XDG_RUNTIME_DIR=/run/user/" + str(uid),
           "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/" + str(uid) + "/bus",
           "NODE_OPTIONS=--require=" + meta["home"] + "/entry-observer.cjs",
           "CI=1", "npm_config_cache=" + meta["home"] + "/npm-cache"]
    return ["sudo", "-n", "-u", user, "--", "env", "-i", *env,
            "/usr/bin/python3", meta["home"] + "/driver.py", "flow", action, meta["prefix"]]


def run_actor(meta, action, output, timeout):
    result = subprocess.run(actor(meta, action), cwd="/", capture_output=True, text=True, timeout=timeout)
    (output / ("actor-" + action + ".log")).write_text(result.stdout + result.stderr)
    print(result.stdout, end="", flush=True)
    if result.returncode:
        print(result.stderr, end="", file=sys.stderr, flush=True)
        raise RuntimeError("Task actor " + action + " failed; retained actor diagnostics")


def export(meta, output):
    # Read evidence as its unprivileged owner, never arbitrary files as root.
    result = command(actor(meta, "export"), cwd="/", timeout=30)
    for name, text in json.loads(result.stdout).items():
        if not re.fullmatch(r"[a-z0-9][a-z0-9_.-]*", name):
            raise RuntimeError("Unexpected evidence filename")
        (output / name).write_text(text)


def prepare(prefix, output, candidate):
    guard()
    output.mkdir(mode=0o700)
    run_id = os.environ["GITHUB_RUN_ID"]
    if not run_id.isdigit() or prefix != Path("/opt/ocm-pr568-" + run_id):
        raise RuntimeError("Requires this run's exclusive installation prefix")
    user = "ocm568-" + run_id
    if subprocess.run(["id", user], capture_output=True).returncode == 0:
        raise RuntimeError("Task user already exists; preserve and reconcile it")
    home = "/home/" + user
    meta = {"user": user, "home": home, "prefix": str(prefix), "createdUser": False,
            "candidateSha256": sha(candidate), "sourceHead": command(["git", "rev-parse", "HEAD"]).stdout.strip(),
            "sourceTree": command(["git", "rev-parse", "HEAD^{tree}"]).stdout.strip(),
            "cargoLockSha256": sha("Cargo.lock"), "openclawVersion": OPENCLAW,
            "predecessorRelease": RELEASE}
    write_json(output / "setup.json", meta)
    downloads = prefix / "downloads"
    downloads.mkdir()
    for name, expected in ASSETS.items():
        data = urllib.request.urlopen("https://github.com/openclaw/ocm/releases/download/" + RELEASE + "/" + name, timeout=120).read()
        if hashlib.sha256(data).hexdigest() != expected:
            raise RuntimeError("Published asset digest changed: " + name)
        (downloads / name).write_bytes(data)
    installer_home = output / "installer-home"
    installer_home.mkdir(mode=0o700)
    command(["bash", downloads / "install.sh", "--version", RELEASE, "--prefix", prefix],
            env={**os.environ, "HOME": str(installer_home)})
    meta["predecessorSha256"] = sha(prefix / "bin/ocm")
    if meta["predecessorSha256"] != "ffd7c3b58b8d73974f102f006a652a58f1d60e0d59afeb8ae8a16ce3db0512ca":
        raise RuntimeError("Unexpected released predecessor executable")
    shutil.copy2(prefix / "bin/ocm", prefix / "predecessor-ocm")
    node_name = "node-v22.15.0-linux-x64.tar.xz"
    node_url = "https://nodejs.org/dist/v22.15.0/"
    manifest = urllib.request.urlopen(node_url + "SHASUMS256.txt", timeout=60).read()
    expected = {line.split()[1]: line.split()[0] for line in manifest.decode().splitlines()}[node_name]
    data = urllib.request.urlopen(node_url + node_name, timeout=120).read()
    if hashlib.sha256(data).hexdigest() != expected:
        raise RuntimeError("Older ambient Node checksum mismatch")
    archive = downloads / node_name
    archive.write_bytes(data)
    tools = prefix / "tools"
    tools.mkdir()
    with tarfile.open(archive) as tar:
        for member in tar.getmembers():
            if Path(member.name).is_absolute() or Path(member.name).parts[0] != "node-v22.15.0-linux-x64" or ".." in Path(member.name).parts:
                raise RuntimeError("Unexpected Node archive path")
        tar.extractall(tools, filter="data")
    meta["ambientBin"] = str(tools / "node-v22.15.0-linux-x64/bin")
    meta["ambientNodeSha256"] = sha(Path(meta["ambientBin"]) / "node")
    meta["ambientArchiveSha256"] = expected
    meta["nodeManifestSha256"] = hashlib.sha256(manifest).hexdigest()
    meta["cpuAffinity"] = sorted(os.sched_getaffinity(0))
    meta["platform"] = platform.platform()
    meta["meminfo"] = Path("/proc/meminfo").read_text()
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        meta["port"] = sock.getsockname()[1]
    command(["sudo", "-n", "useradd", "--create-home", "--user-group", "--home-dir", home, "--shell", "/bin/bash", user])
    meta["createdUser"] = True
    meta["uid"] = int(command(["id", "-u", user]).stdout)
    write_json(output / "setup.json", meta)
    write_json(prefix / "proof.json", meta)
    command(["sudo", "-n", "install", "-o", user, "-g", user, "-m", "0700", Path(__file__).resolve(), home + "/driver.py"])
    command(["sudo", "-n", "loginctl", "enable-linger", user])
    command(["sudo", "-n", "systemctl", "start", "user@" + str(meta["uid"]) + ".service"])
    try:
        run_actor(meta, "before", output, 1500)
    finally:
        export(meta, output)


def flow(action, prefix):
    meta = json.loads((prefix / "proof.json").read_text())
    home = Path.home()
    if str(home) != meta["home"] or os.getuid() != meta["uid"]:
        raise RuntimeError("Wrong task user/home")
    os.chdir(home)
    for name in ["tmp", "proof"]:
        (home / name).mkdir(mode=0o700, exist_ok=True)
    evidence = home / "proof"
    config = home / "environment/.openclaw/openclaw.json"
    binary = prefix / "bin/ocm"
    observer = home / "entry-observer.cjs"
    if action == "before":
        # Node's public preload mechanism observes argv before process.title can
        # overwrite /proc/<pid>/cmdline. It changes no product/runtime objects.
        observer.write_text(
            "const fs=require('node:fs'),path=require('node:path');\n"
            "const root=" + json.dumps(str(home / "entry-receipts")) + ";\n"
            "if(require('node:worker_threads').isMainThread){\n"
            "fs.mkdirSync(root,{recursive:true,mode:0o700});\n"
            "fs.writeFileSync(path.join(root,process.pid+'.json'),JSON.stringify({"
            "pid:process.pid,argv:process.argv,cwd:process.cwd(),execPath:process.execPath}),{flag:'wx'});\n"
            "}\n"
        )

    def scrub(text):
        if config.exists():
            token = json.loads(config.read_text()).get("gateway", {}).get("auth", {}).get("token")
            if isinstance(token, str) and token:
                text = text.replace(token, "[redacted task token]")
        return re.sub(r"(?im)^.*(?:agent model:|modelRuntime|providerApiKey).*$", "[model configuration omitted]", text)

    def failure_output(text):
        try:
            value = json.loads(text)
        except ValueError:
            return scrub(text)
        def redact(value):
            if isinstance(value, dict):
                return {k: "[redacted diagnostic field]" if re.search(r"model|provider|token|secret|api.?key", k, re.I) else redact(v) for k, v in value.items()}
            if isinstance(value, list):
                return [redact(v) for v in value]
            return scrub(value) if isinstance(value, str) else value
        return json.dumps(redact(value), indent=2)

    def run(label, args, timeout=120, require=True, record=True):
        started = time.monotonic()
        result = subprocess.run([str(a) for a in args], capture_output=True, text=True, timeout=timeout)
        if record:
            (evidence / (label + ".log")).write_text(scrub(result.stdout + result.stderr))
        elif result.returncode:
            # Retain failures emitted on either stream, without private fields.
            (evidence / (label + "-failure.log")).write_text(failure_output(result.stdout) + "\n" + scrub(result.stderr))
        row = {"step": label, "exit": result.returncode, "seconds": round(time.monotonic() - started, 3)}
        print(json.dumps(row), flush=True)
        if require and result.returncode:
            raise RuntimeError(label + " failed; see retained sanitized diagnostics")
        return result

    def oc(label, *args, **kwargs):
        return run(label, [binary, *args], **kwargs)

    def rpc(label, method, *args):
        result = oc(label, "env", "run", ENV_NAME, "--", "gateway", "call", method, *args, "--json", record=False, require=False)
        if result.returncode:
            if label == "during-prewarm-health":
                run("prewarm-old-cli-control", [prefix / "predecessor-ocm", "env", "run", ENV_NAME, "--", "gateway", "call", "health", "--json"], record=False, require=False)
                oc("prewarm-service-status", "service", "status", ENV_NAME, "--json", require=False)
                with urllib.request.urlopen("http://127.0.0.1:" + str(meta["port"]) + "/healthz", timeout=5) as response:
                    write_json(evidence / "prewarm-http-health.json", {"status": response.status})
            raise RuntimeError(label + " failed; see sanitized failure output and controls")
        value = json.loads(result.stdout)
        if method == "health":
            assert value["ok"] is True, "Authenticated Gateway health failed"
            write_json(evidence / (label + ".json"), {"ok": True})
        return value

    def session(value):
        rows = value.get("sessions", [])
        return next({k: x[k] for k in ["key", "sessionId", "label", "lifecycleRevision"] if k in x}
                    for x in rows if x.get("key") == SESSION_KEY)

    def checkpoint(phase):
        write_json(evidence / "phase.json", {"action": action, "phase": phase})

    def snapshot(label, expected_node):
        status = json.loads(oc(label + "-status", "service", "status", ENV_NAME, "--json").stdout)
        assert status["running"] and status["gatewayState"] == "running", status
        rpc(label + "-health", "health")
        checkpoint(label + "-http-health")
        with urllib.request.urlopen("http://127.0.0.1:" + str(meta["port"]) + "/healthz", timeout=5) as response:
            assert response.status == 200
        daemon = status["ocmServicePid"]
        child = status["childPid"]
        processes = {}
        for key, pid in [("daemon", daemon), ("child", child)]:
            checkpoint(label + "-" + key + "-proc-executable")
            exe = Path("/proc") / str(pid) / "exe"
            processes[key] = {"pid": pid, "executable": os.readlink(exe), "sha256": sha(exe)}
        assert processes["daemon"]["sha256"] == sha(binary)
        checkpoint(label + "-selected-node-version")
        selected = command([Path("/proc") / str(child) / "exe", "--version"]).stdout.strip()
        assert selected == "v" + expected_node, selected
        assert expected_node in processes["child"]["executable"]
        assert command([Path(meta["ambientBin"]) / "node", "--version"]).stdout.strip() == "v22.15.0"
        checkpoint(label + "-entry-receipt")
        receipt = json.loads((home / "entry-receipts" / (str(child) + ".json")).read_text())
        processes["child"]["observedArgv"] = receipt["argv"]
        entry = Path(receipt["argv"][1]).resolve()
        assert entry.name == "openclaw.mjs" and entry.is_relative_to(home / "store/runtimes")
        assert str(entry) in status["args"], "Running Node entry differs from OCM's selected package"
        package = entry.parent / "package.json"
        assert json.loads(package.read_text())["version"] == OPENCLAW
        checkpoint(label + "-gateway-probe")
        probe = json.loads(oc(label + "-gateway-probe", "env", "run", ENV_NAME, "--", "gateway", "probe", "--json", record=False).stdout)
        target = next(t for t in probe["targets"] if t["connect"]["ok"]
                      and urlsplit(t["url"]).port == meta["port"]
                      and urlsplit(t["url"]).hostname in {"127.0.0.1", "localhost", "::1"})
        assert target["server"]["version"] == OPENCLAW, target["server"]
        package_identity = {"entry": str(entry), "entrySha256": sha(entry), "packageSha256": sha(package),
                            "version": OPENCLAW, "reportedGatewayVersion": target["server"]["version"],
                            "entryObserverSha256": sha(observer)}
        unit = run(label + "-unit", ["systemctl", "--user", "cat", UNIT]).stdout
        assert str(binary) in unit and str(home / "store") in unit and meta["ambientBin"] in unit
        mode = oc(label + "-config-read", "env", "run", ENV_NAME, "--", "config", "get", "gateway.mode", "--json")
        assert json.loads(mode.stdout) == "local"
        data = oc(label + "-data-read", "env", "exec", ENV_NAME, "--", "/bin/cat", home / "environment/.openclaw/workspace/existing.txt")
        assert data.stdout == "Existing data survives OCM and private Node upgrade.\n"
        value = {"configSha256": sha(config), "userDataSha256": sha(home / "environment/.openclaw/workspace/existing.txt"),
                 "ocmSha256": sha(binary), "processes": processes, "selectedNode": selected,
                 "openclawArtifact": package_identity,
                 "session": session(rpc(label + "-sessions", "sessions.list")),
                 "ambientNode": "v22.15.0", "servicePath": status["binaryPath"], "port": meta["port"]}
        write_json(evidence / (label + ".json"), value)
        return value

    try:
        if action == "export":
            print(json.dumps({p.name: p.read_text() for p in evidence.iterdir() if p.is_file()}))
        elif action == "before":
            run("native-user-manager", ["systemctl", "--user", "show-environment"])
            oc("predecessor-version", "--version")
            oc("predecessor-runtime", "runtime", "install", "--version", OPENCLAW, "--json", timeout=1200)
            oc("predecessor-env", "start", ENV_NAME, "--root", home / "environment", "--version", OPENCLAW,
               "--port", meta["port"], "--no-service", "--json", timeout=120)
            workspace = home / "environment/.openclaw/workspace"
            workspace.mkdir(parents=True, exist_ok=True)
            (workspace / "existing.txt").write_text("Existing data survives OCM and private Node upgrade.\n")
            oc("predecessor-install-service", "service", "install", ENV_NAME, "--json")
            # No readiness override, warm retry, or timeout extension.
            started = json.loads(oc("predecessor-start-service", "service", "start", ENV_NAME, "--json").stdout)
            assert started["gatewayReady"] is True, started
            patched = rpc("predecessor-create-session", "sessions.patch", "--params",
                          json.dumps({"key": SESSION_KEY, "label": SESSION_LABEL}))
            assert patched["ok"] and patched["entry"]["label"] == SESSION_LABEL
            before = snapshot("before", "24.21.0")
            assert before["ocmSha256"] == meta["predecessorSha256"]
            print("HEALTHY_RELEASED_PREDECESSOR_ESTABLISHED", flush=True)
        elif action == "after":
            assert sha(binary) == meta["candidateSha256"] != meta["predecessorSha256"]
            before = json.loads((evidence / "before.json").read_text())
            oc("candidate-prewarm", "runtime", "install", "--version", OPENCLAW, "--json", timeout=600)
            # runtime install republishes the bound service plan. A changed Node
            # executable causes an asynchronous restart, so use the public normal
            # readiness boundary before requiring RPC health.
            ready = json.loads(oc("prewarm-service-ready", "service", "start", ENV_NAME, "--json").stdout)
            assert ready["gatewayReady"] is True, ready
            rpc("during-prewarm-health", "health")
            oc("candidate-daemon-refresh", "service", "refresh-daemon", "--acknowledge-gateway-restarts", "--json")
            for label in ["after", "restart"]:
                restarted = json.loads(oc(label + "-service-restart", "service", "restart", ENV_NAME, "--json").stdout)
                assert restarted["gatewayReady"] is True, restarted
                observed = snapshot(label, "24.22.1")
                for key in ["configSha256", "userDataSha256", "session", "openclawArtifact"]:
                    assert observed[key] == before[key], (key, before[key], observed[key])
            write_json(evidence / "verdict.json", {"pass": True, "route": "published installer -> Cargo source install --force, same prefix",
                       "predecessor": RELEASE, "candidateHead": meta["sourceHead"], "candidateTree": meta["sourceTree"],
                       "candidateSha256": sha(binary), "healthyPredecessor": True, "twoCandidateRestartsHealthy": True,
                       "privateNode": "24.21.0 -> 24.22.1", "ambientNode": "22.15.0",
                       "configAndUserDataBytePreserved": True, "durableSessionPreserved": True,
                       "limits": "Native Linux x86_64; same OpenClaw2026.10.5 package. Not public self-update/release/fleet delivery."})
        elif action == "cleanup":
            oc("cleanup-stop", "service", "stop", ENV_NAME, require=False)
            oc("cleanup-uninstall", "service", "uninstall", ENV_NAME, require=False)
        else:
            raise RuntimeError("Unknown flow action")
    except Exception:
        failure = scrub(traceback.format_exc())
        (evidence / ("failure-" + action + ".log")).write_text(failure)
        print(failure, file=sys.stderr, flush=True)
        raise SystemExit(1)
    finally:
        if action != "export":
            run("diagnostic-journal-" + action, ["journalctl", "--user", "-u", UNIT, "--no-pager"], require=False)
            for path in (home / "store").rglob("*.log"):
                if path.is_file():
                    name = hashlib.sha256(str(path.relative_to(home)).encode()).hexdigest()[:12]
                    (evidence / ("product-" + action + "-" + name + ".log")).write_text(scrub(path.read_text(errors="replace")))


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "flow":
        flow(sys.argv[2], Path(sys.argv[3]))
        return
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=["prepare", "verify", "cleanup"])
    parser.add_argument("--prefix", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--candidate", type=Path)
    args = parser.parse_args()
    guard()
    if args.action == "prepare":
        prepare(args.prefix, args.output, args.candidate)
        return
    if args.action == "cleanup" and not (args.output / "setup.json").exists():
        return
    meta = json.loads((args.output / "setup.json").read_text())
    if not meta["createdUser"]:
        return
    try:
        run_actor(meta, "after" if args.action == "verify" else "cleanup", args.output, 900)
    finally:
        try:
            export(meta, args.output)
        finally:
            if args.action == "cleanup":
                command(["sudo", "-n", "loginctl", "disable-linger", meta["user"]])
                terminated = subprocess.run(["sudo", "-n", "loginctl", "terminate-user", meta["user"]], capture_output=True, text=True)
                (args.output / "cleanup-user-manager.log").write_text(terminated.stdout + terminated.stderr)
                # Disabling lingering may already have removed the user manager.
                # Accept that only when the actual owned-process postcondition holds.
                deadline = time.monotonic() + 30
                while True:
                    remaining = subprocess.run(["pgrep", "-u", str(meta["uid"])], capture_output=True, text=True)
                    if remaining.returncode == 1:
                        break
                    if remaining.returncode != 0 or time.monotonic() >= deadline:
                        raise RuntimeError("Task processes remain or process inspection failed: " + remaining.stdout + remaining.stderr)
                    time.sleep(1)
                write_json(args.output / "cleanup.json", {"ownedServiceStopped": True, "taskUserTerminated": True,
                           "privateState": "Retained only on disposable runner until destruction; not uploaded"})


if __name__ == "__main__":
    main()
