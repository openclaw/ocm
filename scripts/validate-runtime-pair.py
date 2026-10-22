INPUT_FILES = ["Cargo.lock", "src/managed_node.rs", "tests/windows_dev_stop_tests.rs", "tests/support/windows_dev_stop.cjs", "tests/support/dev_ui.cjs"]
"""Finite native Windows comparisons; no publication or installed-user state."""
import ctypes
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

candidate = Path(sys.argv[1]).resolve()
base = Path(sys.argv[2]).resolve()
output = Path(sys.argv[3]).resolve()
output.mkdir(parents=True, exist_ok=False)
if sys.platform != "win32":
    raise SystemExit("Native Windows is required")
kernel = ctypes.WinDLL("kernel32", use_last_error=True)
kernel.GetCurrentProcess.restype = ctypes.c_void_p
process = kernel.GetCurrentProcess()
process_mask = ctypes.c_size_t()
system_mask = ctypes.c_size_t()
kernel.GetProcessAffinityMask.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_size_t), ctypes.POINTER(ctypes.c_size_t)]
kernel.SetProcessAffinityMask.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
if not kernel.GetProcessAffinityMask(process, ctypes.byref(process_mask), ctypes.byref(system_mask)):
    raise ctypes.WinError(ctypes.get_last_error())
# Every comparison inherits the same two logical processors on this one runner.
selected = 0
for bit in range(process_mask.value.bit_length()):
    if process_mask.value & (1 << bit):
        selected |= 1 << bit
        if selected.bit_count() == 2:
            break
if not kernel.SetProcessAffinityMask(process, selected):
    raise ctypes.WinError(ctypes.get_last_error())
results = []
identities = {"platform": sys.platform, "available_cpu_mask": process_mask.value,
              "selected_cpu_mask": selected, "logical_cpus": os.cpu_count(),
              "candidate": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=candidate, text=True).strip(),
              "base": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=base, text=True).strip()}

def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()

identities["harness_sha256"] = digest(__file__)

nodes = {"old": Path(os.environ["PAIR_OLD_NODE"]), "new": Path(os.environ["PAIR_NEW_NODE"])}
identities["runtimes"] = {name: {"version": subprocess.check_output([str(node), "--version"], text=True).strip(),
                                  "sha256": digest(node)} for name, node in nodes.items()}
expected_versions = {'old': 'v22.15.0', 'new': 'v22.23.4'}
if {name: value["version"] for name, value in identities["runtimes"].items()} != expected_versions:
    raise SystemExit("Runtime preparation mismatch; comparison is not a product verdict")
# A shared synthetic-fixture diagnostic captures pre-entry errors without
# changing either production revision or exposing product child stderr.
fixture = "tests/support/windows_dev_stop.cjs"
identities["base_pristine_fixture_sha256"] = digest(base / fixture)
(base / fixture).write_bytes((candidate / fixture).read_bytes())
identities["shared_fixture_overlay_sha256"] = digest(base / fixture)
identities["input_hashes"] = {}
for label, repo in [("base", base), ("candidate", candidate)]:
    identities["input_hashes"][label] = {name: digest(repo / name) for name in INPUT_FILES}
(output / "identity.json").write_text(json.dumps(identities, indent=2))

def run(label, command, repo, runtime, timeout=900):
    state = output / (label + "-state")
    state.mkdir()
    # Only operating-system/tool paths reach the tested process; no Actions token.
    env = {key: value for key, value in os.environ.items()
           if key.upper() in {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "TEMP", "TMP", "PROCESSOR_ARCHITECTURE", "RUSTUP_HOME", "CARGO_HOME", "USERPROFILE", "LOCALAPPDATA", "APPDATA",
                               "PROGRAMFILES", "PROGRAMFILES(X86)", "PROGRAMW6432", "SYSTEMDRIVE",
                               "INCLUDE", "LIB", "LIBPATH", "VCINSTALLDIR", "VCTOOLSINSTALLDIR",
                               "VSINSTALLDIR", "WINDOWSSDKDIR", "WINDOWSSDKVERSION",
                               "UNIVERSALCRTSDKDIR", "UCRTVERSION", "VCTOOLSVERSION",
                               "VSCMD_ARG_TGT_ARCH", "VSCMD_ARG_HOST_ARCH", "VSCMD_VER", "VS170COMNTOOLS"}}
    env.update(HOME=str(state), OCM_HOME=str(state / "ocm-home"),
               CI="1", npm_config_cache=str(state / "npm-cache"), PATH=str(nodes[runtime].parent) + os.pathsep + env.get("PATH", ""))
    began = time.monotonic()
    log = output / (label + ".log")
    timed_out = False
    with log.open("wb") as stream:
        child = subprocess.Popen(command, cwd=repo, env=env, stdout=stream, stderr=subprocess.STDOUT)
        try:
            code = child.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            subprocess.run(["taskkill", "/PID", str(child.pid), "/T", "/F"], stdout=stream, stderr=subprocess.STDOUT, check=False)
            code = child.wait(timeout=30)
    result = {"label": label, "runtime": runtime, "command": command, "exit": code,
              "timeout": timed_out, "seconds": time.monotonic() - began, "log_sha256": digest(log)}
    results.append(result)
    (output / "results.json").write_text(json.dumps(results, indent=2))
    print(json.dumps(result), flush=True)
    return code == 0 and not timed_out

executables = {}
for label, repo in [("base", base), ("candidate", candidate)]:
    # Distinct target directories; compile once, then cross the runtime variable.
    if not run(label + "-compile", ["cargo", "test", "--locked", "--test", "windows_dev_stop_tests", "--no-run", "--message-format=json"], repo, "new", 1200):
        raise SystemExit("Compilation failed; lifecycle comparison is not a product verdict")
    records = []
    for line in (output / (label + "-compile.log")).read_text(errors="replace").splitlines():
        try:
            item = json.loads(line)
        except json.JSONDecodeError:
            continue
        if item.get("reason") == "compiler-artifact" and item.get("executable") and item.get("target", {}).get("name") == "windows_dev_stop_tests":
            records.append(item["executable"])
    if len(records) != 1:
        raise SystemExit("Expected exactly one lifecycle test executable")
    executables[label] = records[0]
    identities[label + "_test_binary_sha256"] = digest(records[0])
    identities[label + "_ocm_binary_sha256"] = digest(repo / "target/debug/ocm.exe")
(output / "identity.json").write_text(json.dumps(identities, indent=2))
for round_no in range(2):
    order = [("base", "old"), ("candidate", "new"), ("base", "new"), ("candidate", "old")]
    if round_no % 2:
        order.reverse()
    for source, runtime in order:
        repo = base if source == "base" else candidate
        run(f"{source}-{runtime}-lifecycle-{round_no}", [executables[source], "--exact", "native_windows_dev_stop_lifecycle", "--nocapture"], repo, runtime, 210)
# Real official ZIP installation and offline reuse for each revision's selected managed Node.
for source, repo in [("base", base), ("candidate", candidate)]:
    run(source + "-managed-zip", ["cargo", "test", "--locked", "--lib", "managed_node::tests::windows_managed_node_zip_installs_and_reuses_offline", "--", "--ignored", "--exact", "--nocapture"], repo, "new", 600)
raise SystemExit(1 if any(result["exit"] != 0 or result["timeout"] for result in results) else 0)
