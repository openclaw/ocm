"""Production CLI proof with real Git/signing/gh and a local GitHub service.

No production credential, repository, release or service is touched. Only the
GitHub API is a fixture; the CLI, existing verification scripts, Git transport,
tag signature verification, gh JSON queries and dispatch HTTP request are real.
"""

import http.server
import json
import os
from pathlib import Path
import shutil
import socketserver
import subprocess
import sys
import tempfile
import threading
import unittest
from urllib.parse import parse_qs, urlsplit

ROOT = Path(__file__).resolve().parents[2]
REPO = "openclaw/ocm"
MARKER = "<!-- ocm-auto-release:v1 -->"
JOBS = ("Format", "Rust 1.88 minimum", "Windows compile",
        "Test (ubuntu-latest)", "Test (macos-latest)",
        "npm (ubuntu-latest, Node 22.15.0)", "npm (ubuntu-latest, Node 24)",
        "npm (macos-15-intel, Node 24)", "npm (macos-15, Node 24)")


class ReleaseBoundaryTests(unittest.TestCase):
    def setUp(self):
        for tool in ("git", "gh", "ssh-keygen", "cargo", "perl"):
            self.assertIsNotNone(shutil.which(tool), f"Boundary proof requires {tool}")
        directory = tempfile.TemporaryDirectory(prefix="ocm-release-boundary-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.repo = self.root / "checkout"
        self.repo.mkdir()
        self.remote = self.root / "remote.git"
        # Deliberate environment allowlist: never inherit tokens or credentials.
        self.env = {key: os.environ[key] for key in ("PATH", "HOME", "TMPDIR")
                    if key in os.environ}
        self.env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
                        GH_CONFIG_DIR=str(self.root / "gh"), GH_PROMPT_DISABLED="1",
                        GH_ENTERPRISE_TOKEN="inert-loopback-fixture",
                        OCM_GH_BIN=shutil.which("gh"))
        self.command("git", "init", "--bare", str(self.remote))
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Release Fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        key = self.root / "signer"
        self.command("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key))
        allowed = self.root / "allowed-signers"
        allowed.write_text("fixture@example.invalid " + key.with_suffix(".pub").read_text())
        self.git("config", "gpg.format", "ssh")
        self.git("config", "user.signingkey", str(key))
        self.git("config", "gpg.ssh.allowedSignersFile", str(allowed))
        self.git("remote", "add", "origin", f"https://github.com/{REPO}.git")
        self.git("config", f"url.{self.remote.as_uri()}.insteadOf", f"https://github.com/{REPO}.git")
        scripts = self.repo / "scripts"
        scripts.mkdir()
        for name in ("auto-release.py", "verify-release-tag.sh", "verify-release-ci.sh",
                     "read-package-version.sh", "validate-version.sh", "update-version.sh"):
            shutil.copy2(ROOT / "scripts" / name, scripts / name)
        (self.repo / "src").mkdir()
        (self.repo / "src/main.rs").write_text("fn main() {}\n")
        self.version("0.2.37")
        self.commit("fixture base")
        self.version("0.2.38")
        self.base = self.commit("chore(release): bump version to 0.2.38 (#1)")
        self.git("tag", "-s", "v0.2.38", "-m", "v0.2.38")
        (self.repo / "src/main.rs").write_text('fn main() { println!("fix"); }\n')
        self.product = self.commit("fix: eligible product change")
        self.prs = {1: self.release_pr(1, "0.2.38", self.base)}
        self.closed = []
        self.requests = []
        self.effects = []
        self.dispatched = False
        self.api_errors = []
        self.serve()

    def command(self, *args, check=True):
        result = subprocess.run(args, cwd=self.repo, env=self.env, text=True,
                                capture_output=True, timeout=60)
        if check:
            self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def git(self, *args):
        return self.command("git", *args).stdout.strip()

    def version(self, version):
        (self.repo / "Cargo.toml").write_text(
            f'[package]\nname = "ocm"\nversion = "{version}"\nedition = "2024"\n')
        (self.repo / "Cargo.lock").write_text(
            f'version = 4\n\n[[package]]\nname = "ocm"\nversion = "{version}"\n')

    def commit(self, title):
        self.git("add", ".")
        self.git("commit", "-m", title)
        return self.git("rev-parse", "HEAD")

    def release_pr(self, number, version, sha, automated=True):
        return {"number": number, "user": {"login": "release-bot" if automated else "maintainer"},
                "body": MARKER if automated else "Manual release", "merged": True,
                "state": "closed", "merged_at": "2026-01-01T00:00:00Z",
                "base": {"ref": "main"}, "head": {"ref": f"release/v{version}",
                "sha": sha, "repo": {"full_name": REPO}}, "merge_commit_sha": sha,
                "title": f"chore(release): bump version to {version}"}

    def serve(self):
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass  # Never log request headers (including inert authentication).

            def respond(self):
                path = urlsplit(self.path)
                body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                payload = json.loads(body) if body else None
                fixture.requests.append((self.command, path.path))
                if self.command != "GET":
                    fixture.effects.append((self.command, path.path, payload))
                try:
                    result = fixture.route(path.path, parse_qs(path.query), self.command, payload)
                    data = json.dumps(result).encode() if result is not None else b""
                    self.send_response(200 if data else 204)
                except Exception as error:
                    fixture.api_errors.append(str(error))
                    data = b'{"message":"unexpected fixture request"}'
                    self.send_response(500)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            do_GET = respond
            do_POST = respond
            do_PUT = respond

        # gh's documented Unix-socket transport cannot connect to public GitHub.
        # Keep the pathname short enough for macOS's sockaddr_un limit.
        socket_dir = tempfile.TemporaryDirectory(prefix="ocm-gh-", dir="/tmp")
        self.addCleanup(socket_dir.cleanup)
        socket_path = str(Path(socket_dir.name) / "api.sock")
        server = socketserver.ThreadingUnixStreamServer(socket_path, Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        self.env["GH_HOST"] = "release-fixture.invalid"
        self.command("gh", "config", "set", "http_unix_socket", socket_path)

    def route(self, path, query, method, payload):
        prefix = f"/api/v3/repos/{REPO}"
        if path == "/api/v3/user":
            return {"login": "release-bot"}
        if path == "/api/v3/meta":
            return {"installed_version": "3.16.0"}
        self.assertTrue(path.startswith(prefix), f"Unexpected API route: {path}")
        endpoint = path[len(prefix):].strip("/")
        if endpoint == "":
            return {"default_branch": "main", "full_name": REPO}
        if endpoint == "releases":
            return [{"tag_name": "v0.2.38", "draft": False, "prerelease": False}]
        if endpoint == "git/ref/heads/main":
            return {"object": {"sha": self.head, "type": "commit"}}
        if endpoint.startswith("git/ref/tags/"):
            tag = endpoint.removeprefix("git/ref/tags/")
            return {"object": {"type": "tag", "sha": self.git("--git-dir", str(self.remote),
                                                               "rev-parse", f"refs/tags/{tag}")}}
        if endpoint.startswith("git/tags/"):
            obj = endpoint.removeprefix("git/tags/")
            tag = next(line[4:] for line in self.git("cat-file", "-p", obj).splitlines()
                       if line.startswith("tag "))
            verified = self.command("git", "verify-tag", obj, check=False).returncode == 0
            return {"tag": tag, "object": {"type": "commit", "sha": self.git("rev-parse", obj + "^{}")},
                    "verification": {"verified": verified}}
        if endpoint.startswith("git/matching-refs/"):
            prefix_ref = "refs/" + endpoint.removeprefix("git/matching-refs/")
            refs = self.git("--git-dir", str(self.remote), "for-each-ref", "--format=%(refname) %(objectname)")
            return [{"ref": name, "object": {"sha": sha}} for name, sha in
                    (line.split() for line in refs.splitlines()) if name.startswith(prefix_ref)]
        if endpoint.startswith("compare/"):
            base, head = endpoint.removeprefix("compare/").split("...")
            self.git("merge-base", "--is-ancestor", base, head)
            return {"status": "identical" if base == head else "ahead"}
        if endpoint.startswith("commits/") and endpoint.endswith("/pulls"):
            sha = endpoint.split("/")[1]
            return [pr for pr in self.prs.values() if pr["merge_commit_sha"] == sha]
        if endpoint == "pulls":
            if method == "GET":
                return self.closed if query.get("state") == ["closed"] else []
            raise AssertionError("Unexpected PR creation")
        if endpoint.startswith("pulls/"):
            self.assertEqual(method, "GET", "Unexpected PR mutation")
            return self.prs[int(endpoint.split("/")[1])]
        if endpoint == "actions/workflows/ci.yml/runs":
            return {"workflow_runs": [{"id": 1, "head_sha": query["head_sha"][0],
                    "head_branch": "main", "event": "push", "head_repository": {"full_name": REPO},
                    "status": "completed", "conclusion": "success",
                    "html_url": f"https://github.com/{REPO}/actions/runs/1"}]}
        if endpoint == "actions/runs/1/jobs":
            return {"total_count": len(JOBS), "jobs": [{"name": name, "conclusion": "success"} for name in JOBS]}
        if endpoint == "actions/workflows/release.yml/runs":
            return {"workflow_runs": ([{"display_title": "Release v0.2.39", "head_branch": "main",
                    "head_repository": {"full_name": REPO}, "status": "in_progress"}] if self.dispatched else [])}
        if endpoint == "actions/workflows/release.yml":
            return {"id": 2, "name": "Release", "path": ".github/workflows/release.yml", "state": "active"}
        if endpoint in {"actions/workflows/2/dispatches", "actions/workflows/release.yml/dispatches"}:
            self.assertEqual(method, "POST")
            self.assertEqual(payload, {"ref": "main", "inputs": {"tag": "v0.2.39"}})
            self.dispatched = True
            return None
        raise AssertionError(f"Unhandled fixture route: {endpoint}")

    def publish_fixture_main(self):
        self.head = self.git("rev-parse", "HEAD")
        self.git("push", "origin", "main", "--tags")

    def cli(self):
        result = self.command(sys.executable, "-B", "scripts/auto-release.py", check=False)
        self.assertEqual(self.api_errors, [], self.api_errors)
        return result

    def refs(self):
        return self.git("--git-dir", str(self.remote), "show-ref")

    def test_automated_merge_signs_exact_commit_pushes_and_dispatches_once(self):
        self.version("0.2.39")
        target = self.commit("chore(release): bump version to 0.2.39 (#2)")
        self.prs[2] = self.release_pr(2, "0.2.39", target)
        self.publish_fixture_main()
        result = self.cli()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Dispatched existing signed-release workflow", result.stdout)
        self.assertEqual(self.git("--git-dir", str(self.remote), "rev-parse", "v0.2.39^{}"), target)
        self.git("verify-tag", "v0.2.39")
        self.assertEqual(len(self.effects), 1, self.effects)
        first_refs = self.refs()
        retry = self.cli()
        self.assertEqual(retry.returncode, 0, retry.stderr)
        self.assertIn("Release build already running", retry.stdout)
        self.assertEqual(self.refs(), first_refs)
        self.assertEqual(len(self.effects), 1, "Retry must not dispatch again")

    def test_manual_merge_has_no_sign_push_or_dispatch_effect(self):
        self.version("0.2.39")
        target = self.commit("chore(release): bump version to 0.2.39 (#2)")
        self.prs[2] = self.release_pr(2, "0.2.39", target, automated=False)
        self.publish_fixture_main()
        before = self.refs()
        result = self.cli()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("manually owned", result.stderr)
        self.assertEqual(self.refs(), before)
        self.assertEqual(self.git("tag", "--list", "v0.2.39"), "")
        self.assertEqual(self.effects, [])

    def test_closed_proposal_with_deleted_branch_is_not_recreated(self):
        self.closed = [{"number": 2, "state": "closed", "merged": False}]
        self.publish_fixture_main()
        before = self.refs()
        result = self.cli()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("proposal was closed", result.stderr)
        self.assertEqual(self.refs(), before)
        self.assertEqual(self.git("tag", "--list", "v0.2.39"), "")
        self.assertEqual(self.effects, [])

    def test_closed_proposal_with_retained_branch_is_not_recreated(self):
        self.git("checkout", "-b", "release/v0.2.39")
        self.version("0.2.39")
        self.git("add", "Cargo.toml", "Cargo.lock")
        self.git("commit", "-S", "-m", "chore(release): bump version to 0.2.39", "-m", MARKER)
        self.git("push", "origin", "release/v0.2.39")
        self.git("checkout", "main")
        self.test_closed_proposal_with_deleted_branch_is_not_recreated()


if __name__ == "__main__":
    unittest.main()
