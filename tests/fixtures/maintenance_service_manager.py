#!/usr/bin/env python3
"""Service observation boundary for isolated maintenance CLI tests."""
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

home = Path(os.environ["OCM_HOME"])
case = home.parent
args = sys.argv[1:]
target_name = os.environ.get("OCM_TEST_MAINTENANCE_TARGET", "source")
loaded = case / "launchctl-print.txt"
runtime_path = home / "supervisor/runtime.json"
with (case / "launchctl.log").open("a") as log:
    log.write(" ".join(args) + "\n")
if args[0] == "bootstrap":
    loaded.write_text(f"state = running\npid = {os.environ['OCM_TEST_NATIVE_DAEMON_PID']}\npath = {args[-1]}\n")
if args[0] in ("bootout", "unload"):
    loaded.unlink(missing_ok=True)
    sys.exit(0)
if args[0] == "print":
    registry_path = home / "envs.json"
    registry = json.loads(registry_path.read_text())["envs"] if registry_path.exists() else []
    target = next((row for row in registry if row["name"] == target_name), None)
    state_path = home / "supervisor/state.json"
    plan = json.loads(state_path.read_text())["children"] if state_path.exists() else []
    fault = ((case / "armed").exists() and target and not target["serviceRunning"]
             and not any(row["envName"] == target_name for row in plan))
    mode = (case / "mode").read_text()
    if mode == "post-update-error":
        witness = home / "envs" / target_name / ".openclaw/source-witness"
        fault = witness.exists() and witness.read_text() == "native-result"
    if mode == "after-checkpoint-error":
        fault = len(list((home / "snapshots" / target_name).glob("*.json"))) > 1
    if fault:
        (case / "observed-fault").write_text(mode)
        if mode in ("manager-error", "post-update-error", "after-checkpoint-error"):
            print("fixture observation unavailable", file=sys.stderr)
            sys.exit(1)
    children = [dict(envName=s["envName"], bindingKind=s["bindingKind"], bindingName=s["bindingName"],
                     pid=4242 if s["envName"] == target_name else 4243, restartCount=0,
                     childPort=s["childPort"], stdoutPath=str(case / "stdout"),
                     stderrPath=str(case / "stderr")) for s in plan] if loaded.exists() else []
    runtime = dict(kind="ocm-supervisor-runtime", ocmHome=str(home), daemonVersion="0.3.4",
                   gatewayAdmission=json.loads((case / "identity.json").read_text()),
                   updatedAt=datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
                   children=children,
                   services=[dict(c, gatewayState="running", restartHandoff="none") for c in children])
    if fault and mode == "foreign-store":
        runtime["ocmHome"] = str(case / "other-store")
    if fault and mode == "stale-process":
        runtime["gatewayAdmission"]["process"]["startedAt"] = "stale"
    runtime_path.parent.mkdir(parents=True, exist_ok=True)
    temporary = runtime_path.with_suffix(".fixture-tmp")
    temporary.write_text("malformed" if fault and mode == "malformed" else json.dumps(runtime))
    temporary.replace(runtime_path)
    if fault and mode == "missing":
        runtime_path.unlink()
    if loaded.exists():
        print(loaded.read_text(), end="")
        sys.exit(0)
    print(f'Could not find service "{args[-1].rsplit("/", 1)[-1]}" in domain for user gui', file=sys.stderr)
    sys.exit(1)
sys.exit(0)
