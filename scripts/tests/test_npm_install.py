"""Real npm installs against a task-local, read-only fixture registry."""

from contextlib import contextmanager, redirect_stdout
from datetime import datetime, timezone
import hashlib
import io
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import platform
import re
import selectors
import signal
import subprocess
import sys
import tarfile
import tempfile
import time
from threading import Thread
import unittest
from unittest import mock
from urllib.parse import unquote

from test_npm_release import release, stage_fixture


@contextmanager
def registry(directories):
    versions = {}
    tarballs = {}
    for directory in directories:
        receipt = release.read_receipt(directory)
        for entry in receipt["packages"]:
            with tarfile.open(directory / entry["file"]) as tar:
                manifest = json.load(tar.extractfile("package/package.json"))
            versions[entry["version"]] = manifest
            tarballs[entry["file"]] = (directory / entry["file"]).read_bytes()
            manifest["dist"] = {
                "integrity": entry["integrity"],
                "tarball": entry["file"],
            }

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            path = unquote(self.path.split("?")[0])
            filename = path.rsplit("/", 1)[-1]
            if filename in tarballs:
                body = tarballs[filename]
                content_type = "application/octet-stream"
            elif path == "/@openclaw/ocm":
                body = release.json_bytes(
                    {
                        "name": release.PACKAGE,
                        "versions": versions,
                        "dist-tags": {
                            "latest": max(
                                (
                                    version
                                    for version in versions
                                    if not any(
                                        version.endswith(f"-{p}")
                                        for p in release.TARGETS
                                    )
                                ),
                                key=release.version_key,
                            )
                        },
                    }
                )
                content_type = "application/json"
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    url = f"http://127.0.0.1:{server.server_port}"
    for manifest in versions.values():
        manifest["dist"]["tarball"] = f"{url}/tarballs/{manifest['dist']['tarball']}"
    thread = Thread(target=server.serve_forever)
    thread.start()
    try:
        yield url
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def isolated_env(root, url):
    home = root / "home"
    home.mkdir()
    return {
        "PATH": os.environ["PATH"],
        "HOME": str(home),
        "OCM_HOME": str(root / "ocm-home"),
        "npm_config_cache": str(root / "cache"),
        "npm_config_userconfig": str(root / "empty-npmrc"),
        "npm_config_registry": url,
        "npm_config_audit": "false",
        "npm_config_fund": "false",
        "npm_config_update_notifier": "false",
        "npm_config_ignore_scripts": "true",
        "NO_COLOR": "1",
    }


def run(command, cwd, env, *, notarization_sha256=None, **kwargs):
    started = datetime.now(timezone.utc)
    result = subprocess.run(
        command, cwd=cwd, env=env, text=True, capture_output=True, timeout=90, **kwargs
    )
    ended = datetime.now(timezone.utc)
    if notarization_sha256 is not None:
        report_notarization_lookup_errors(result, notarization_sha256, started, ended)
    if result.returncode:
        raise AssertionError(
            f"{command} exited {result.returncode}\n{result.stdout}\n{result.stderr}"
        )
    return result


def installed_native(prefix):
    matches = list(
        prefix.glob(
            "lib/node_modules/@openclaw/ocm/node_modules/@openclaw/ocm-*/vendor/*/bin/ocm"
        )
    )
    if len(matches) != 1:
        raise AssertionError(f"expected one compatible native payload, found {matches}")
    return matches[0]


def macos_team_id(value):
    team = (value or "").strip()
    if re.fullmatch(r"[A-Z0-9]{10}", team):
        return team
    return None


def macos_signature_command(value):
    team_id = macos_team_id(value)
    if team_id:
        return ["--team-id", team_id, "--require-notarization"]
    raise AssertionError(
        "expected macOS signer must be a 10-character Apple Developer Team ID "
        "(MACOS_TEAM_ID for publication)"
    )


class NpmInstallTests(unittest.TestCase):
    def test_smoke_rejects_changed_installed_bytes_before_execution(self):
        with tempfile.TemporaryDirectory(prefix="ocm-npm-tamper-") as temporary:
            packages, _ = stage_fixture(
                Path(temporary), binary=b"#!/bin/sh\nprintf '1.0.0\\n'\n"
            )
            find_native = installed_native

            def change_installed_bytes(prefix):
                binary = find_native(prefix)
                with binary.open("ab") as file:
                    file.write(b"\n# changed after npm installation\n")
                return binary

            with mock.patch(
                f"{__name__}.installed_native", side_effect=change_installed_bytes
            ):
                with self.assertRaisesRegex(
                    AssertionError, "npm install changed native executable bytes"
                ):
                    smoke(packages, expected_macos_team_id="AB12CD34EF")

    def test_real_global_upgrade_reinstall_local_npx_and_missing_optionals(self):
        with tempfile.TemporaryDirectory(prefix="ocm-npm-test-") as temporary:
            root = Path(temporary)
            first, _ = stage_fixture(root, "1.0.0", b"#!/bin/sh\nprintf 'one\\n'\n")
            second, _ = stage_fixture(root, "2.0.0", b"#!/bin/sh\nprintf 'two\\n'\n")
            with registry([first, second]) as url:
                env = isolated_env(root, url)
                prefix = root / "prefix"
                entrypoint = prefix / "bin/ocm"
                run(
                    [
                        "npm",
                        "install",
                        "--global",
                        "--prefix",
                        str(prefix),
                        "@openclaw/ocm@1.0.0",
                    ],
                    root,
                    env,
                )
                native = installed_native(prefix)
                self.assertEqual(
                    run([str(entrypoint)], root, env).stdout.strip(), "one"
                )
                run(
                    [
                        "npm",
                        "install",
                        "--global",
                        "--prefix",
                        str(prefix),
                        "@openclaw/ocm@2.0.0",
                    ],
                    root,
                    env,
                )
                self.assertEqual(installed_native(prefix), native)
                self.assertEqual(
                    run([str(entrypoint)], root, env).stdout.strip(), "two"
                )
                run(
                    [
                        "npm",
                        "uninstall",
                        "--global",
                        "--prefix",
                        str(prefix),
                        "@openclaw/ocm",
                    ],
                    root,
                    env,
                )
                run(
                    [
                        "npm",
                        "install",
                        "--global",
                        "--prefix",
                        str(prefix),
                        "@openclaw/ocm@2.0.0",
                    ],
                    root,
                    env,
                )
                self.assertEqual(installed_native(prefix), native)
                self.assertEqual(run([str(native)], root, env).stdout.strip(), "two")
                local = root / "project"
                local.mkdir()
                (local / "package.json").write_text('{"private":true}')
                run(["npm", "install", "@openclaw/ocm@2.0.0"], local, env)
                self.assertEqual(
                    run(
                        [str(local / "node_modules/.bin/ocm")], local, env
                    ).stdout.strip(),
                    "two",
                )
                self.assertEqual(
                    run(
                        [
                            "npm",
                            "exec",
                            "--yes",
                            "--package=@openclaw/ocm@2.0.0",
                            "--",
                            "ocm",
                        ],
                        root,
                        env,
                    ).stdout.strip(),
                    "two",
                )
                omitted = root / "omitted"
                run(
                    [
                        "npm",
                        "install",
                        "--prefix",
                        str(omitted),
                        "--omit=optional",
                        "@openclaw/ocm@2.0.0",
                    ],
                    root,
                    env,
                )
                missing = subprocess.run(
                    [str(omitted / "node_modules/.bin/ocm")],
                    cwd=root,
                    env=env,
                    text=True,
                    capture_output=True,
                )
                self.assertNotEqual(missing.returncode, 0)
                self.assertIn("optional dependencies", missing.stderr)

    def test_launcher_preserves_arguments_stdin_exit_and_signal_identity(self):
        with tempfile.TemporaryDirectory(prefix="ocm-npm-streams-") as temporary:
            root = Path(temporary)
            script = b"""#!/bin/sh
case "$1" in
  signal) kill -TERM $$ ;;
  pid) printf '%s\\n' "$$"; exec sleep 30 ;;
  *) printf '<%s>\\n' "$@"; cat; exit 17 ;;
esac
"""
            packages, _ = stage_fixture(root, binary=script)
            with registry([packages]) as url:
                env = isolated_env(root, url)
                prefix = root / "prefix"
                run(
                    [
                        "npm",
                        "install",
                        "--global",
                        "--prefix",
                        str(prefix),
                        "@openclaw/ocm@1.0.0",
                    ],
                    root,
                    env,
                )
                entrypoint = prefix / "bin/ocm"
                result = subprocess.run(
                    [str(entrypoint), "--color", "never", "space value", ""],
                    input="stdin bytes\n",
                    text=True,
                    capture_output=True,
                    env=env,
                    timeout=10,
                )
                self.assertEqual(result.returncode, 17)
                self.assertEqual(
                    result.stdout,
                    "<--color>\n<never>\n<space value>\n<>\nstdin bytes\n",
                )
                result = subprocess.run(
                    [str(entrypoint), "signal"],
                    env=env,
                    capture_output=True,
                    timeout=10,
                )
                self.assertEqual(result.returncode, -signal.SIGTERM)
                child = subprocess.Popen(
                    [str(entrypoint), "pid"], env=env, stdout=subprocess.PIPE, text=True
                )
                try:
                    self.assertEqual(int(child.stdout.readline()), child.pid)
                    child.terminate()
                    self.assertEqual(child.wait(timeout=10), -signal.SIGTERM)
                finally:
                    if child.poll() is None:
                        child.kill()
                        child.wait()
                    child.stdout.close()

    def test_unsupported_target_and_missing_execve_fail_clearly(self):
        wrapper = str(release.ROOT / "npm/ocm.cjs")
        for override, message in [
            (
                "Object.defineProperty(process, 'platform', {value:'win32'});",
                "Unsupported OCM platform",
            ),
            ("process.execve = undefined;", "requires Node.js"),
        ]:
            result = subprocess.run(
                ["node", "-e", override + "require(process.argv[1])", wrapper],
                text=True,
                capture_output=True,
                timeout=10,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(message, result.stderr)


class MacosTeamIdTests(unittest.TestCase):
    def test_accepts_a_ten_character_team_id(self):
        self.assertEqual(macos_team_id("AB12CD34EF"), "AB12CD34EF")

    def test_rejects_empty_missing_and_whitespace(self):
        self.assertIsNone(macos_team_id(""))
        self.assertIsNone(macos_team_id(None))
        self.assertIsNone(macos_team_id("   "))

    def test_rejects_wrong_shape(self):
        self.assertIsNone(macos_team_id("ab12cd34ef"))
        self.assertIsNone(macos_team_id("ABC"))
        self.assertIsNone(macos_team_id("ABCDEFGHIJK"))

    def test_valid_team_id_always_verifies(self):
        expected = ["--team-id", "AB12CD34EF", "--require-notarization"]
        self.assertEqual(macos_signature_command("AB12CD34EF"), expected)
        self.assertEqual(macos_signature_command(" AB12CD34EF\n"), expected)

    def test_publication_requires_its_own_valid_team_id_before_install(self):
        with tempfile.TemporaryDirectory(prefix="ocm-npm-team-") as temporary:
            missing = Path(temporary) / "missing-packages"
            for value in [None, "", "   ", "invalid", "ABCDEFGHIJK"]:
                with self.subTest(value=value), mock.patch.dict(os.environ):
                    os.environ.pop("MACOS_TEAM_ID", None)
                    if value is not None:
                        os.environ["MACOS_TEAM_ID"] = value
                    with mock.patch.object(sys, "platform", "darwin"):
                        with self.assertRaisesRegex(AssertionError, "MACOS_TEAM_ID"):
                            smoke(missing)

    def test_fixture_signer_does_not_depend_on_publication_configuration(self):
        with tempfile.TemporaryDirectory(prefix="ocm-npm-team-") as temporary:
            missing = Path(temporary) / "missing-packages"
            for value in [None, "", "invalid", "ZZ99YY88XX"]:
                with self.subTest(value=value), mock.patch.dict(os.environ):
                    os.environ.pop("MACOS_TEAM_ID", None)
                    if value is not None:
                        os.environ["MACOS_TEAM_ID"] = value
                    with mock.patch.object(sys, "platform", "darwin"):
                        with self.assertRaises(FileNotFoundError) as failure:
                            smoke(missing, expected_macos_team_id="AB12CD34EF")
                    self.assertEqual(
                        failure.exception.filename, str(missing / "release.json")
                    )

    def test_invalid_explicit_signer_never_falls_back_to_publication(self):
        with tempfile.TemporaryDirectory(prefix="ocm-npm-team-") as temporary:
            missing = Path(temporary) / "missing-packages"
            with mock.patch.dict(os.environ, {"MACOS_TEAM_ID": "AB12CD34EF"}):
                with mock.patch.object(sys, "platform", "darwin"):
                    for value in ["", "   ", "invalid"]:
                        with self.subTest(value=value):
                            with self.assertRaisesRegex(AssertionError, "10-character"):
                                smoke(missing, expected_macos_team_id=value)


# Diagnostic data is advisory and never participates in the verifier's verdict.
NOTARIZATION_LOG_LIMIT = 64 * 1024
NOTARIZATION_ERROR_LIMIT = 12


def notarization_attempts(stderr):
    if len(stderr) > NOTARIZATION_LOG_LIMIT:
        raise ValueError("oversized verifier output")
    attempts = []
    for line in stderr.splitlines():
        start = re.fullmatch(r"OCM_NOTARIZATION_START ([1-3]) ([1-9][0-9]{0,9})", line)
        end = re.fullmatch(r"OCM_NOTARIZATION_END ([1-3]) ([0-9]{1,3})", line)
        if start:
            number, pid = map(int, start.groups())
            if number != len(attempts) + 1 or (attempts and "status" not in attempts[-1]):
                raise ValueError("invalid attempt order")
            if any(item["pid"] == pid for item in attempts):
                raise ValueError("reused attempt PID")
            attempts.append({"attempt": number, "pid": pid})
        elif end:
            number, status = map(int, end.groups())
            if not attempts or number != len(attempts) or "status" in attempts[-1] or status > 255:
                raise ValueError("invalid attempt status")
            attempts[-1]["status"] = status
    if any("status" not in attempt for attempt in attempts):
        raise ValueError("incomplete attempt")
    return attempts


def notarization_log(command):
    # Bound capture while reading, not after unbounded communicate()/JSON input.
    # This collector runs only after the existing verification child settles.
    deadline = time.monotonic() + 15
    data = bytearray()
    with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as child:
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(child.stdout, selectors.EVENT_READ)
                while True:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0 or not selector.select(remaining):
                        raise ValueError("diagnostic timeout")
                    chunk = os.read(child.stdout.fileno(), 8192)
                    if not chunk:
                        break
                    if len(data) + len(chunk) > NOTARIZATION_LOG_LIMIT:
                        raise ValueError("diagnostic too large")
                    data.extend(chunk)
            if child.wait(timeout=max(0, deadline - time.monotonic())):
                raise ValueError("diagnostic failed")
        finally:
            if child.poll() is None:
                child.kill()
            child.wait()
    return json.loads(data)


def report_notarization_lookup_errors(result, binary_sha256, started, ended):
    # Publish allowlisted fields only; raw captured/system output stays private.
    try:
        if not re.fullmatch(r"[0-9a-f]{64}", binary_sha256):
            raise ValueError("invalid digest")
        attempts = notarization_attempts(result.stderr)
        print("macOS verification: " + json.dumps({
            "binary_sha256": binary_sha256,
            "started_utc": started.isoformat(), "ended_utc": ended.isoformat(),
            "status": result.returncode, "attempts": attempts,
        }, sort_keys=True))
        if not attempts:
            print("Notarization diagnostic unavailable: no observed attempts")
            return
        pids = " OR ".join(f'processID == {attempt["pid"]}' for attempt in attempts)
        entries = notarization_log([
            "/usr/bin/log", "show", "--last", "2m", "--style", "json",
            "--predicate",
            'process == "codesign" AND subsystem == "com.apple.securityd" '
            'AND eventMessage CONTAINS "Error checking with notarization daemon:" '
            f"AND ({pids})",
        ])
        if not isinstance(entries, list):
            raise ValueError("unexpected log response")
        errors = []
        for entry in entries:
            if not isinstance(entry, dict):
                continue
            pid, timestamp, message = (
                entry.get("processID"), entry.get("timestamp"), entry.get("eventMessage")
            )
            if type(pid) is not int or not isinstance(timestamp, str) or not isinstance(message, str):
                continue
            attempt = next((item for item in attempts if item["pid"] == pid), None)
            match = re.fullmatch(r"Error checking with notarization daemon: (-?[0-9]{1,10})", message.strip())
            if attempt is None or match is None:
                continue
            try:
                observed = datetime.fromisoformat(timestamp)
                matches_time = observed.tzinfo is not None and started <= observed <= ended
            except ValueError:
                continue
            code = int(match.group(1))
            if not matches_time or not -(2**31) <= code < 2**31:
                continue
            errors.append({"attempt": attempt["attempt"], "code": code})
            if len(errors) == NOTARIZATION_ERROR_LIMIT:
                break
        print("Correlated notarization lookup errors (up to 12; phase unknown; not a cause): " + json.dumps(errors))
    except (OSError, ValueError, RecursionError, subprocess.TimeoutExpired):
        print("Notarization diagnostic unavailable")


class NotarizationDiagnosticTests(unittest.TestCase):
    digest = "a" * 64
    started = datetime(2026, 1, 1, tzinfo=timezone.utc)
    ended = datetime(2026, 1, 1, 0, 1, tzinfo=timezone.utc)
    markers = "OCM_NOTARIZATION_START 1 123\nOCM_NOTARIZATION_END 1 1\n"

    def report(self, entries, *, status=1, stderr=None):
        result = subprocess.CompletedProcess([], status, "private stdout", stderr or self.markers)
        output = io.StringIO()
        with mock.patch(f"{__name__}.notarization_log", return_value=entries), redirect_stdout(output):
            report_notarization_lookup_errors(result, self.digest, self.started, self.ended)
        return output.getvalue()

    def test_only_correlates_matching_pid_and_time_and_allowlisted_numeric_errors(self):
        def event(**changes):
            return {"processID": 123, "timestamp": "2026-01-01 00:00:30.000000+0000",
                    "eventMessage": "Error checking with notarization daemon: 51", **changes}

        output = self.report([
            event(), event(processID=456), event(timestamp="2001-01-01 00:00:30+0000"),
            event(timestamp="2026-01-01 00:01:01+0000"), event(timestamp="private timestamp"),
            event(timestamp="2026-01-01 00:00:30"), event(processID="123"),
            event(eventMessage="Error checking with notarization daemon: private token"),
            event(eventMessage="private URL/path/token"), None,
        ])
        self.assertIn('[{"attempt": 1, "code": 51}]', output)
        self.assertIn('"pid": 123', output)
        self.assertIn("phase unknown; not a cause", output)
        self.assertNotIn("private", output)
        self.assertNotIn("456", output)
        self.assertNotIn("2001", output)
        many = self.report([event()] * 100)
        self.assertEqual(many.count('"code": 51'), NOTARIZATION_ERROR_LIMIT)
        self.assertLess(len(many), 2048)

    def test_recovered_success_retains_both_attempts_without_raw_output(self):
        markers = self.markers + "OCM_NOTARIZATION_START 2 124\nOCM_NOTARIZATION_END 2 0\nprivate path\n"
        output = self.report([], status=0, stderr=markers)
        record = json.loads(output.splitlines()[0].removeprefix("macOS verification: "))
        self.assertEqual(record["status"], 0)
        self.assertEqual(record["attempts"], [
            {"attempt": 1, "pid": 123, "status": 1}, {"attempt": 2, "pid": 124, "status": 0},
        ])
        self.assertNotIn("private", output)

    def test_malformed_or_unavailable_diagnostics_preserve_run_outcome_and_timeout(self):
        for status in [0, 17]:
            for failure in [ValueError("private"), OSError("private"), RecursionError("private"),
                            subprocess.TimeoutExpired("private", 15)]:
                with self.subTest(status=status, failure=type(failure)):
                    result = subprocess.CompletedProcess(["verifier"], status, "", self.markers)
                    with mock.patch.object(subprocess, "run", return_value=result) as execute, \
                         mock.patch(f"{__name__}.notarization_log", side_effect=failure), \
                         redirect_stdout(io.StringIO()) as output:
                        if status:
                            with self.assertRaisesRegex(AssertionError, "exited 17"):
                                run(["verifier"], None, {}, notarization_sha256=self.digest)
                        else:
                            self.assertIs(run(["verifier"], None, {}, notarization_sha256=self.digest), result)
                    self.assertEqual(execute.call_args.kwargs["timeout"], 90)
                    self.assertIn("Notarization diagnostic unavailable", output.getvalue())
                    self.assertNotIn("private", output.getvalue())
        timeout = subprocess.TimeoutExpired("verifier", 90)
        with mock.patch.object(subprocess, "run", side_effect=timeout), \
             mock.patch(f"{__name__}.notarization_log") as collect:
            with self.assertRaises(subprocess.TimeoutExpired) as raised:
                run(["verifier"], None, {}, notarization_sha256=self.digest)
            self.assertIs(raised.exception, timeout)
            collect.assert_not_called()
        for stderr in [self.markers * 2, self.markers + "OCM_NOTARIZATION_START 2 124\n",
                       "x" * (NOTARIZATION_LOG_LIMIT + 1)]:
            self.assertEqual(self.report([], stderr=stderr), "Notarization diagnostic unavailable\n")
        self.assertIn("diagnostic unavailable", self.report({"private": "data"}))

    def test_collector_bounds_capture_and_settles_its_child(self):
        cases = [
            ("print('[]')", False),
            ("print('x' * 1000000)", True),
            ("print('not json')", True),
            ("raise SystemExit(9)", True),
        ]
        popen = subprocess.Popen
        for script, fails in cases:
            with self.subTest(script=script), mock.patch.object(subprocess, "Popen", wraps=popen) as spawn:
                if fails:
                    with self.assertRaises(ValueError):
                        notarization_log([sys.executable, "-c", script])
                else:
                    self.assertEqual(notarization_log([sys.executable, "-c", script]), [])
        children = []
        def track(*args, **kwargs):
            child = popen(*args, **kwargs)
            children.append(child)
            return child
        # Expire the collector's existing budget immediately after spawning.
        with mock.patch.object(subprocess, "Popen", side_effect=track), \
             mock.patch.object(time, "monotonic", side_effect=[0, 16]):
            with self.assertRaisesRegex(ValueError, "timeout"):
                notarization_log([sys.executable, "-c", "import time; time.sleep(30)"])
        self.assertIsNotNone(children[0].returncode)
        self.assertTrue(children[0].stdout.closed)

    def test_verifier_markers_name_the_codesign_pid_and_recovered_status(self):
        with tempfile.TemporaryDirectory(prefix="ocm-notary-observation-") as temporary:
            root = Path(temporary)
            binary = root / "ocm"
            binary.write_text("fixture")
            codesign = root / "codesign"
            codesign.write_text("""#!/bin/sh
case "$1" in
  -d) printf '%s\n' 'Identifier=com.openclaw.ocm' 'TeamIdentifier=AB12CD34EF' \
      'Authority=Developer ID Application: fixture' 'flags=0x10000(runtime)' 'Timestamp=fixture' ;;
  *) case "$*" in
       *--check-notarization*)
         printf '%s\n' "$$" >> "$TEST_PIDS"
         if [ ! -f "$TEST_ONCE" ]; then touch "$TEST_ONCE"; exit 7; fi ;;
     esac ;;
esac
""")
            codesign.chmod(0o755)
            sleep = root / "sleep"
            sleep.write_text("""#!/bin/sh
printf '%s\\n' "$1" >> "$TEST_DELAYS"
""")
            sleep.chmod(0o755)
            env = {**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"],
                   "TEST_PIDS": str(root / "pids"), "TEST_ONCE": str(root / "once"),
                   "TEST_DELAYS": str(root / "delays")}
            result = subprocess.run([
                str(release.ROOT / "scripts/verify-macos-release.sh"), "--binary", str(binary),
                *macos_signature_command("AB12CD34EF"),
            ], env=env, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            attempts = notarization_attempts(result.stderr)
            self.assertEqual([item["pid"] for item in attempts],
                             list(map(int, (root / "pids").read_text().splitlines())))
            self.assertEqual([item["status"] for item in attempts], [7, 0])
            self.assertEqual((root / "delays").read_text(), "10\n")


def smoke(directory, *, expected_macos_team_id=None):
    signature_command = None
    if sys.platform == "darwin":
        signature_command = macos_signature_command(
            expected_macos_team_id
            if expected_macos_team_id is not None
            else os.environ.get("MACOS_TEAM_ID")
        )
    receipt = release.read_receipt(directory)
    with tempfile.TemporaryDirectory(prefix="ocm-npm-native-") as temporary:
        root = Path(temporary)
        with registry([directory]) as url:
            env = isolated_env(root, url)
            prefix = root / "prefix"
            run(
                [
                    "npm",
                    "install",
                    "--global",
                    "--prefix",
                    str(prefix),
                    f"{release.PACKAGE}@{receipt['version']}",
                ],
                root,
                env,
            )
            binary = installed_native(prefix)
            metadata = json.loads((binary.parents[3] / "release.json").read_bytes())
            binary_sha256 = hashlib.sha256(binary.read_bytes()).hexdigest()
            if binary_sha256 != metadata["binarySha256"]:
                raise AssertionError("npm install changed native executable bytes")
            entrypoint = prefix / "bin/ocm"
            result = run([str(entrypoint), "--version"], root, env)
            if receipt["version"] not in result.stdout:
                raise AssertionError(
                    "installed executable version differs from package"
                )
            run([str(entrypoint), "--help"], root, env)
            if signature_command is not None:
                run(
                    [
                        str(release.ROOT / "scripts/verify-macos-release.sh"),
                        "--binary",
                        str(binary),
                        *signature_command,
                    ],
                    root,
                    env,
                    notarization_sha256=binary_sha256,
                )
                print("Verified macOS signature and notarization after npm installation")
            print(
                f"Verified npm install, CLI, and unchanged native bytes on {platform.platform()}"
            )


def published_binary_fixture():
    # This old release proves signed-byte preservation only. Production prepare
    # rejects it because the native npm ownership guard had not shipped.
    # Public expected signer of this pinned release, verified on both macOS
    # architectures. It is independent of the current publisher's configuration.
    repo, version, team_id = "openclaw/ocm", "0.2.39", "FWJYW4S8P8"
    tag = f"v{version}"
    snapshot = release.release_snapshot(repo, tag)
    with tempfile.TemporaryDirectory(prefix="ocm-npm-release-fixture-") as temporary:
        root = Path(temporary)
        assets = root / "assets"
        assets.mkdir()
        release.download_assets(repo, tag, snapshot, assets)
        output = root / "npm"
        release.stage(
            version,
            {"repository": repo, "tag": tag, "assets": snapshot},
            assets,
            (release.ROOT / "npm/ocm.cjs").read_bytes(),
            (release.ROOT / "LICENSE").read_bytes(),
            (release.ROOT / "npm/README.md").read_bytes(),
            output,
        )
        smoke(output, expected_macos_team_id=team_id)
        if sys.platform == "darwin":
            # Reuse the downloaded fixture to prove a different expected team
            # cannot pass the same verifier used for the installed executable.
            target = release.TARGETS[
                "darwin-arm64" if platform.machine() == "arm64" else "darwin-x64"
            ]
            binary = root / "wrong-team-ocm"
            binary.write_bytes(release.native_bytes(assets / f"ocm-{target}.tar.gz"))
            binary.chmod(0o755)
            wrong_team = "AAAAAAAAAA" if team_id != "AAAAAAAAAA" else "BBBBBBBBBB"
            result = subprocess.run(
                [
                    str(release.ROOT / "scripts/verify-macos-release.sh"),
                    "--binary",
                    str(binary),
                    *macos_signature_command(wrong_team),
                ],
                cwd=root,
                text=True,
                capture_output=True,
                timeout=90,
            )
            if result.returncode == 0 or (
                f"not signed by Apple Developer Team {wrong_team}" not in result.stderr
            ):
                raise AssertionError(
                    "wrong-signer control did not reject the team: "
                    f"{result.stdout}{result.stderr}"
                )
            print("Verified that a different expected macOS signer is rejected")


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--packages":
        smoke(Path(sys.argv[2]).resolve())
    elif sys.argv[1:] == ["--published-binary-fixture"]:
        published_binary_fixture()
    else:
        unittest.main()
