"""Execute the trusted workflow signing wrapper without Apple credentials/network."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

WORKFLOW = Path(__file__).resolve().parents[2] / ".github/workflows/release.yml"
TARGET = "x86_64-apple-darwin"
SUBMISSION = "9306a788-96c3-4260-87f4-97863b0c5ffe"
CDHASH = "a" * 40
REDACTION_MARKER = "synthetic-output-redaction-marker"


def signing_code():
    step = WORKFLOW.read_text().split("      - name: Sign and notarize macOS executable\n", 1)[1]
    run = textwrap.dedent(step.split("        run: |\n", 1)[1].split("      - name:", 1)[0])
    return run.split("<<'PYTHON'\n", 1)[1].rsplit("PYTHON\n", 1)[0]


class SigningDiagnosticsTests(unittest.TestCase):
    def execute(self, status=37, accepted=True, malformed=False, tool_failure=False,
                oversized=False, symlink=False, timeout=False, interrupted=False):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "scripts").mkdir()
            tools = root / "tools"
            tools.mkdir()
            runner = root / "runner"
            runner.mkdir()
            binary = root / "target" / TARGET / "release/ocm"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"exact public-source executable fixture")
            if symlink:
                binary.unlink()
                binary.symlink_to(root / "private-file")
                (root / "private-file").write_text(REDACTION_MARKER)
            signer = root / "scripts/sign-notarize-macos.sh"
            output = "existing signer output\n"
            if accepted:
                output += f"Apple notarization accepted submission {SUBMISSION}\n"
            signer.write_text(f"#!/bin/sh\nprintf '%s' '{output}'\nexit {status}\n")
            signer.chmod(0o755)
            report = {
                "status": "Accepted", "statusCode": 0, "issues": [{"message": REDACTION_MARKER}],
                "ticketContents": [
                    {"cdhash": CDHASH, "arch": "x86_64", "path": "ocm.zip/ocm", "arbitraryText": REDACTION_MARKER},
                    {"cdhash": REDACTION_MARKER, "arch": "x86_64", "path": "ocm.zip/ocm"},
                    {"cdhash": CDHASH, "arch": REDACTION_MARKER, "path": "ocm.zip/ocm"},
                    {"cdhash": CDHASH, "arch": "x86_64", "path": REDACTION_MARKER},
                ], "arbitraryText": REDACTION_MARKER,
            }
            (root / "report.json").write_text(json.dumps(report))
            (root / "workflow.py").write_text(signing_code())
            fake = f'''#!{sys.executable}
import json, os, pathlib, stat, sys
root = pathlib.Path(os.environ["FIXTURE_ROOT"])
name = pathlib.Path(sys.argv[0]).name
with (root / "calls").open("a") as log:
    log.write(name + "\\n")
if name == "codesign":
    print("CDHash={CDHASH}\\nIdentifier=com.openclaw.ocm\\nTeamIdentifier=AB12CD34EF\\nFormat=Mach-O thin (x86_64)\\n{REDACTION_MARKER}", file=sys.stderr)
elif name == "system-log":
    print(json.dumps([{{"eventMessage": "Error checking with notarization daemon: 51"}}, {{"eventMessage": "Error checking with notarization daemon: 3"}}, {{"eventMessage": "Error checking with notarization daemon: {REDACTION_MARKER}"}}, {{"eventMessage": "{REDACTION_MARKER}"}}]))
else:
    key = pathlib.Path(sys.argv[sys.argv.index("--key") + 1])
    assert stat.S_IMODE(key.stat().st_mode) == 0o600
    assert key.read_text() == "{REDACTION_MARKER}"
    (root / "key-path").write_text(str(key))
    print("{REDACTION_MARKER}", file=sys.stderr)
    if os.environ.get("TOOL_FAILURE"):
        print("{REDACTION_MARKER}")
        sys.exit(9)
    if os.environ.get("MALFORMED"):
        print("{REDACTION_MARKER}")
    elif os.environ.get("OVERSIZED"):
        print("x" * 65537)
    else:
        print((root / "report.json").read_text())
'''
            for name in ["codesign", "system-log", "xcrun"]:
                tool = tools / name
                tool.write_text(fake)
                tool.chmod(0o755)
            # The workflow deliberately uses the system log's absolute path;
            # only redirect that OS boundary for a portable isolated fixture.
            bootstrap = '''import os, pathlib, runpy, signal, subprocess, sys
original = subprocess.run
def run(command, **kwargs):
    if command[0] == "/usr/bin/log":
        command[0] = os.environ["FIXTURE_ROOT"] + "/tools/system-log"
    if command[0] == "xcrun" and (os.environ.get("TIMEOUT") or os.environ.get("INTERRUPTED")):
        key = pathlib.Path(command[command.index("--key") + 1])
        pathlib.Path(os.environ["FIXTURE_ROOT"], "key-path").write_text(str(key))
        if os.environ.get("INTERRUPTED"):
            os.kill(os.getpid(), signal.SIGTERM)
        raise subprocess.TimeoutExpired(command, 15)
    return original(command, **kwargs)
subprocess.run = run
sys.argv = ["workflow.py", "x86_64-apple-darwin"]
runpy.run_path("workflow.py", run_name="__main__")
'''
            env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                       FIXTURE_ROOT=str(root), RUNNER_TEMP=str(runner),
                       GITHUB_OUTPUT=str(root / "outputs"), OCM_NOTARY_API_PRIVATE_KEY=REDACTION_MARKER,
                       OCM_NOTARY_API_KEY_ID="SYNTHETIC", OCM_NOTARY_ISSUER_ID="synthetic-issuer")
            for key, value in {"MALFORMED": malformed, "TOOL_FAILURE": tool_failure,
                               "OVERSIZED": oversized, "TIMEOUT": timeout,
                               "INTERRUPTED": interrupted}.items():
                env.pop(key, None)
                if value:
                    env[key] = "1"
            result = subprocess.run([sys.executable, "-c", bootstrap], cwd=root, env=env,
                                    text=True, capture_output=True, timeout=30)
            self.assertEqual(result.returncode, status, result.stderr)
            self.assertNotIn(REDACTION_MARKER, result.stdout + result.stderr)
            self.assertEqual(list(runner.iterdir()), [])
            if (root / "key-path").exists():
                self.assertFalse(Path((root / "key-path").read_text()).exists())
            outputs = (root / "outputs").read_text() if (root / "outputs").exists() else ""
            calls = (root / "calls").read_text() if (root / "calls").exists() else ""
            return result.stdout, outputs, calls, output

    def test_success_preserves_output_and_runs_no_diagnostics(self):
        actual, outputs, calls, expected = self.execute(status=0)
        self.assertEqual(actual, expected)
        self.assertEqual((outputs, calls), ("", ""))

    def test_failure_retains_exit_metadata_and_only_safe_ticket_fields(self):
        actual, outputs, calls, expected = self.execute()
        self.assertTrue(actual.startswith(expected))
        for field in [f"CDHash={CDHASH}", "Identifier=com.openclaw.ocm", "TeamIdentifier=AB12CD34EF",
                      "Format=Mach-O thin (x86_64)", "[51, 3]", '"issueCount": 1']:
            self.assertIn(field, actual)
        report = json.loads(actual.split("Apple notary log diagnostic: ", 1)[1])
        self.assertEqual(report["tickets"], [{"cdhash": CDHASH, "arch": "x86_64", "path": "ocm.zip/ocm"}])
        self.assertEqual(outputs, "diagnostic_binary=true\n")
        self.assertIn("xcrun", calls)

    def test_unavailable_notary_diagnostics_still_fail_and_remove_key(self):
        for mode in ["malformed", "tool_failure", "oversized", "timeout", "interrupted"]:
            with self.subTest(mode=mode):
                actual, _, _, _ = self.execute(**{mode: True})
                if mode != "interrupted":
                    self.assertIn("Apple notary log diagnostic unavailable", actual)

    def test_no_submission_skips_authenticated_request_and_symlink_is_not_retained(self):
        _, outputs, calls, _ = self.execute(accepted=False, symlink=True)
        self.assertEqual(outputs, "")
        self.assertNotIn("xcrun", calls)

    def test_artifact_is_only_failed_binary_and_outside_publisher_inventory(self):
        workflow = WORKFLOW.read_text()
        artifact = workflow.split("      - name: Retain failed macOS executable for diagnosis\n", 1)[1].split("      - name:", 1)[0]
        self.assertIn("failure() && steps.sign-macos.outputs.diagnostic_binary == 'true'", artifact)
        self.assertIn("name: notarization-diagnostic-${{ matrix.target }}-${{ github.run_attempt }}", artifact)
        self.assertIn("path: ./target/${{ matrix.target }}/release/ocm\n", artifact)
        self.assertIn("retention-days: 1", artifact)
        self.assertNotIn("name: release-", artifact)
        self.assertIn("pattern: release-*", workflow)
        self.assertNotIn("continue-on-error:", workflow)
        self.assertIn("ref: ${{ needs.verify.outputs.source }}", workflow)


if __name__ == "__main__":
    unittest.main()
