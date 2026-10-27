#!/usr/bin/env python3
"""Remote macOS only: observe the unchanged foreground regression, fail-first.

Usage: python3 observe-native.py NEW_OUTPUT_ROOT [ATTEMPTS=20]
Run from the exact qualified OCM source checkout with its existing CI toolchain.
No product/test source changes. Each attempt gets its own TMPDIR. The collector
records marker transitions and process state; it never signals a process.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

TEST = "terminal_interrupt_waits_for_descendants_after_background_job_is_foregrounded"
if sys.platform != "darwin":
    raise SystemExit("This check requires the coordinated native macOS runner")
root = Path(sys.argv[1]).resolve()
root.mkdir(parents=True, exist_ok=False)
attempts = int(sys.argv[2]) if len(sys.argv) > 2 else 20
if not 1 <= attempts <= 20:
    raise SystemExit("attempts must be 1..20")
command = ["cargo", "test", "--locked", "--test", "cli_invocation_tests", TEST,
           "--", "--exact", "--nocapture"]
identity = {"bash_path": shutil.which("bash"), "python_path": shutil.which("python3"),
            "ocm_test_binary_override": os.environ.get("OCM_TEST_BINARY")}
for name, cmd in {
    "head": ["git", "rev-parse", "HEAD"],
    "tree": ["git", "rev-parse", "HEAD^{tree}"],
    "source_status": ["git", "status", "--porcelain"],
    "rust": ["rustc", "--version"],
    "kernel": ["uname", "-a"],
    "bash": ["bash", "--version"],
    "python": ["python3", "--version"],
}.items():
    identity[name] = subprocess.check_output(cmd, text=True).strip()
(root / "identity.json").write_text(json.dumps(identity, indent=2) + "\n")
results = []
for attempt in range(1, attempts + 1):
    run = root / str(attempt)
    temporary = run / "tmp"
    temporary.mkdir(parents=True)
    env = dict(os.environ, TMPDIR=str(temporary))
    with (run / "test.log").open("w") as log, (run / "observations.jsonl").open("w") as samples:
        started = time.monotonic()
        child = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT)
        markers = {}
        last = {}
        while child.poll() is None:
            for marker in (temporary / "ocm-tests").glob("cli-background-foreground-drain-*/signals-*.log"):
                markers[str(marker)] = marker
            for key, marker in markers.items():
                try:
                    contents = marker.read_text()
                except FileNotFoundError:
                    continue
                pids = [line.split()[1] for line in contents.splitlines()
                        if line.startswith(("ready ", "owner ")) and len(line.split()) == 2]
                if not pids:
                    continue
                observed = subprocess.run(
                    ["/bin/ps", "-o", "pid=,ppid=,pgid=,tpgid=,stat=", "-p", ",".join(pids)],
                    text=True, capture_output=True, check=False,
                )
                try:
                    after = marker.read_text()
                except FileNotFoundError:
                    after = None
                event = {"marker": marker.name, "contents": contents,
                         "contents_after_ps": after,
                         "ps_status": observed.returncode, "ps": observed.stdout,
                         "ps_error": observed.stderr}
                if last.get(key) != event:
                    samples.write(json.dumps(dict(event, time=time.monotonic() - started)) + "\n")
                    samples.flush()
                    last[key] = event
            time.sleep(0.01)
        result = {"attempt": attempt, "exit": child.returncode,
                  "seconds": time.monotonic() - started}
        results.append(result)
        print(json.dumps(result), flush=True)
    if result["exit"] != 0:
        break
(root / "results.json").write_text(json.dumps(results, indent=2) + "\n")
if results[-1]["exit"]:
    raise SystemExit(results[-1]["exit"])
print("No failure observed; this does not disprove the historical failure.", flush=True)
