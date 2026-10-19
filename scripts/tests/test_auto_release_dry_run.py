"""Exercise the nonpublishing release workflow's exact-main CI gate."""

import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


WORKFLOW = Path(__file__).resolve().parents[2] / ".github/workflows/release.yml"
SHA = "a" * 40


class ReleaseDryRunTests(unittest.TestCase):
    def test_source_is_emitted_only_after_exact_main_ci_passes(self):
        workflow = WORKFLOW.read_text()
        step = workflow.split("      - name: Verify merged main for dry run\n", 1)[1]
        command = textwrap.dedent(step.split("        run: |\n", 1)[1].split("\n  build:", 1)[0])
        for result_code in (0, 37):
            with self.subTest(result_code=result_code), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / "scripts").mkdir()
                verifier = root / "scripts/verify-release-ci.sh"
                verifier.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > args\nexit "$RESULT_CODE"\n')
                verifier.chmod(0o755)
                output = root / "output"
                output.touch()
                env = {
                    "PATH": os.environ["PATH"], "GITHUB_SHA": SHA,
                    "GITHUB_OUTPUT": str(output), "RELEASE_REPO": "openclaw/ocm",
                    "RESULT_CODE": str(result_code),
                }
                result = subprocess.run(["bash", "-e", "-c", command], cwd=root, env=env,
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, result_code)
                self.assertEqual((root / "args").read_text().splitlines(),
                                 ["--repo", "openclaw/ocm", "--commit", SHA])
                self.assertEqual(output.read_text(), f"source={SHA}\n" if result_code == 0 else "")

    def test_dry_run_cannot_enter_publication_or_replace_a_release_run(self):
        workflow = WORKFLOW.read_text()
        publish = workflow.split("\n  publish:\n", 1)[1]
        self.assertIn("    if: ${{ !inputs.dry_run }}\n", publish.split("    steps:\n", 1)[0])
        self.assertIn("run-name: ${{ inputs.dry_run && 'Dry run' || 'Release' }}", workflow)
        signed_tag = workflow.split("      - name: Verify signed tag and package version\n", 1)[1]
        self.assertTrue(signed_tag.startswith("        if: ${{ !inputs.dry_run }}\n"))

    def test_npm_dry_run_keeps_native_checks_and_skips_publication(self):
        workflow = WORKFLOW.with_name("publish-npm.yml").read_text()
        self.assertIn("dry_run:", workflow)
        publish = workflow.split("\n  publish:\n", 1)[1]
        self.assertIn("!inputs.dry_run &&", publish.split("    steps:\n", 1)[0])
        self.assertIn("needs: [prepare, native]", publish)
        self.assertIn("run-name: ${{ inputs.dry_run && 'Dry run npm' || 'Publish npm' }}", workflow)


if __name__ == "__main__":
    unittest.main()
