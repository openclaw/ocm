"""Release policy and Git/GitHub boundary tests. No network or real publishing."""

import copy
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

SOURCE = Path(__file__).resolve().parents[1] / "auto-release.py"
spec = importlib.util.spec_from_file_location("auto_release", SOURCE)
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)
A, B, C = "a" * 40, "b" * 40, "c" * 40


class PolicyTests(unittest.TestCase):
    def test_versions(self):
        self.assertEqual(release.next_version("0.2.38", "auto"), "0.2.39")
        self.assertEqual(release.next_version("0.2.38", "minor"), "0.3.0")
        self.assertEqual(release.next_version("0.2.38", "major"), "1.0.0")
        for value in ["0.2.38-rc.1", "01.2.3", "v1.2.3", "1.2", "1.2.3\n", "1.2.3;exit"]:
            with self.subTest(value=value), self.assertRaises(release.ReleaseError):
                release.next_version(value, "auto")

    def test_documentation_allowlist_does_not_hide_shipped_scripts_or_skills(self):
        self.assertTrue(release.docs_only(["docs/RELEASING.md", "README.md"]))
        for paths in [[], ["skills/foo/SKILL.md"], ["scripts/release.sh"],
                      [".github/workflows/ci.yml"], ["README.md", "src/main.rs"]]:
            self.assertFalse(release.docs_only(paths))

    def test_breaking_changes_need_explicit_minor_or_major(self):
        for message in ["feat!: new format", "fix(state)!: migrate", "fix: x\n\nBREAKING CHANGE: schema",
                        "feat: x\nBREAKING-CHANGE: schema"]:
            self.assertTrue(release.needs_explicit_bump([message]))
        self.assertFalse(release.needs_explicit_bump(["fix: checkpoint logs", "docs: explain BREAKING CHANGE examples"]))


class GitVersionTests(unittest.TestCase):
    package_name = "ocm"

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.root_patch = patch.object(release, "ROOT", self.root)
        self.root_patch.start()
        # Do not inherit the operator's signer, git hooks, or credential config.
        self.environment = patch.dict(os.environ, {
            "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_AUTHOR_NAME": "Release test", "GIT_AUTHOR_EMAIL": "test@example.invalid",
            "GIT_COMMITTER_NAME": "Release test", "GIT_COMMITTER_EMAIL": "test@example.invalid"})
        self.environment.start()
        self.r = release.Reconciler()
        self.r.git("init", "-b", "main")
        (self.root / "scripts").mkdir()
        for name in ["read-package-version.sh", "update-version.sh", "validate-version.sh"]:
            target = self.root / "scripts" / name
            target.write_bytes((SOURCE.parent / name).read_bytes())
            target.chmod(0o755)
        other_name = "ocm" if self.package_name == "openclawocm" else "openclawocm"
        (self.root / "Cargo.toml").write_text(
            '[lib]\nname = "ocm"\npath = "src/lib.rs"\n\n'
            f'[package]\nname = "{self.package_name}"\nversion = "0.2.38"\n'
            '\n[[bin]]\nname = "ocm"\npath = "src/main.rs"\n')
        (self.root / "Cargo.lock").write_text(
            f'version = 4\n\n[[package]]\nname = "{other_name}"\nversion = "0.1.0"\n'
            'source = "registry+https://github.com/rust-lang/crates.io-index"\n'
            f'\n[[package]]\nname = "{self.package_name}"\nversion = "0.2.38"\n')
        self.r.git("add", ".")
        self.r.git("commit", "-m", "initial")
        self.base = self.r.git("rev-parse", "HEAD")
        self.r.main = self.base

    def tearDown(self):
        self.environment.stop()
        self.root_patch.stop()
        self.directory.cleanup()

    def commit_version(self):
        self.r.script("update-version.sh", "0.2.39")
        self.r.git("add", "Cargo.toml", "Cargo.lock")
        self.r.git("commit", "-m", "version")
        return self.r.git("rev-parse", "HEAD")

    def test_real_version_commit_is_version_only_and_checkout_is_untouched(self):
        commit = self.r.create_version_commit("0.2.39")
        self.assertEqual(self.r.verify_version_diff(commit, "0.2.39"), self.base)
        self.assertEqual(self.r.git("rev-parse", "HEAD"), self.base)
        self.assertEqual(self.r.git("status", "--porcelain"), "")
        self.assertEqual(self.r.version_at(commit), "0.2.39")

    def test_rejects_dependency_changes_in_version_files(self):
        self.commit_version()
        with (self.root / "Cargo.toml").open("a") as stream:
            stream.write('\n[dependencies]\nevil = "1"\n')
        self.r.git("add", "Cargo.toml")
        self.r.git("commit", "--amend", "--no-edit")
        with self.assertRaisesRegex(release.ReleaseError, "beyond"):
            self.r.verify_version_diff(self.r.git("rev-parse", "HEAD"), "0.2.39")

    def test_rejects_non_version_file_and_trailing_whitespace_changes(self):
        self.commit_version()
        with (self.root / "Cargo.lock").open("a") as stream:
            stream.write(" \n")
        self.r.git("add", "Cargo.lock")
        self.r.git("commit", "--amend", "--no-edit")
        with self.assertRaises(release.ReleaseError):
            self.r.verify_version_diff(self.r.git("rev-parse", "HEAD"), "0.2.39")
        (self.root / "payload").write_text("code")
        self.r.git("add", "payload")
        self.r.git("commit", "--amend", "--no-edit")
        with self.assertRaisesRegex(release.ReleaseError, "only the two"):
            self.r.verify_version_diff(self.r.git("rev-parse", "HEAD"), "0.2.39")

    def test_rejects_target_changes_alongside_the_version(self):
        self.commit_version()
        manifest = self.root / "Cargo.toml"
        manifest.write_text(manifest.read_text().replace('path = "src/main.rs"', 'path = "src/other.rs"'))
        self.r.git("add", "Cargo.toml")
        self.r.git("commit", "--amend", "--no-edit")
        with self.assertRaisesRegex(release.ReleaseError, "beyond"):
            self.r.verify_version_diff(self.r.git("rev-parse", "HEAD"), "0.2.39")


class RenamedGitVersionTests(GitVersionTests):
    package_name = "openclawocm"


class CIGateTests(unittest.TestCase):
    def setUp(self):
        self.r = release.Reconciler()
        self.ci = {"id": 42, "head_sha": A, "head_branch": "main", "event": "push",
                   "head_repository": {"full_name": release.REPO}, "status": "completed",
                   "conclusion": "success", "html_url": "https://github.com/openclaw/ocm/actions/runs/42"}
        names = ("Format", "Rust 1.88 minimum", "Windows compile",
                 "Test (ubuntu-latest)", "Test (macos-latest)",
                 "npm (ubuntu-latest, Node 22.15.0)", "npm (ubuntu-latest, Node 24)",
                 "npm (macos-15-intel, Node 24)", "npm (macos-15, Node 24)")
        self.jobs = {"total_count": len(names), "jobs": [
            {"name": name, "conclusion": "success"} for name in names]}

    def check(self, runs, jobs=None):
        with patch.object(release, "api", side_effect=[{"workflow_runs": runs}, jobs or self.jobs]):
            return self.r.ci(A, "main", "push")

    def test_exact_main_ci_all_jobs_pass(self):
        self.assertTrue(self.check([self.ci]))

    def test_missing_running_wrong_sha_wrong_branch_and_fork_do_not_pass(self):
        self.assertFalse(self.check([]))
        for key, value in [("head_sha", B), ("head_branch", "feature"),
                           ("event", "pull_request"), ("status", "in_progress"),
                           ("head_repository", {"full_name": "attacker/ocm"})]:
            run = dict(self.ci, **{key: value})
            self.assertFalse(self.check([run]))

    def test_failed_ambiguous_missing_and_skipped_jobs_stop_release(self):
        with self.assertRaises(release.ReleaseError):
            self.check([dict(self.ci, conclusion="failure")])
        with self.assertRaises(release.ReleaseError):
            self.check([self.ci, self.ci])
        for outcome in ["skipped", "failure", None]:
            jobs = copy.deepcopy(self.jobs)
            jobs["jobs"][0]["conclusion"] = outcome
            with self.assertRaises(release.ReleaseError):
                self.check([self.ci], jobs)
        with self.assertRaises(release.ReleaseError):
            self.check([self.ci], {"total_count": self.jobs["total_count"],
                                   "jobs": self.jobs["jobs"][:-1]})

    def test_missing_failed_or_replaced_npm_jobs_stop_release(self):
        for index, job in enumerate(self.jobs["jobs"]):
            if not job["name"].startswith("npm ("):
                continue
            for outcome in ["missing", "failure", "skipped", None, "renamed", "duplicate"]:
                with self.subTest(job=job["name"], outcome=outcome):
                    jobs = copy.deepcopy(self.jobs)
                    if outcome == "missing":
                        jobs["jobs"].pop(index)
                        jobs["total_count"] -= 1
                    elif outcome == "renamed":
                        jobs["jobs"][index]["name"] = "unexpected npm job"
                    elif outcome == "duplicate":
                        jobs["jobs"][index]["name"] = "Format"
                    else:
                        jobs["jobs"][index]["conclusion"] = outcome
                    with self.assertRaisesRegex(release.ReleaseError, "every required release job"):
                        self.check([self.ci], jobs)


class ReconciliationTests(unittest.TestCase):
    def setUp(self):
        self.r = release.Reconciler()
        self.r.actor = "release-bot"
        self.r.main = A
        self.pr = {"number": 10, "head": {"ref": "release/v0.2.39", "sha": B,
                   "repo": {"full_name": release.REPO}}, "base": {"ref": "main"},
                   "user": {"login": "release-bot"}, "body": release.MARKER,
                   "title": "chore(release): bump version to 0.2.39", "html_url": "pr/10"}
        self.published = [{"tag_name": "v0.2.38", "draft": False, "prerelease": False}]

    def test_manual_release_pr_and_multiple_releases_are_not_adopted(self):
        for prs in [[dict(self.pr, body="manual")], [dict(self.pr, user={"login": "human"})],
                    [self.pr, self.pr]]:
            with patch.object(release, "pages", return_value=prs), self.assertRaises(release.ReleaseError):
                self.r.pending()

    def test_already_published_release_does_not_dispatch_or_tag(self):
        with patch.object(release, "api") as api, patch.object(release, "run") as run:
            self.assertFalse(self.r.finish_release("0.2.38", self.published))
            api.assert_not_called()
            run.assert_not_called()

    def setup_reconcile(self, pr=None, base=A, ready=True):
        self.r.git = Mock(side_effect=lambda *a: {
            "status": "", "config": f"https://github.com/{release.REPO}.git",
            "rev-parse": C, "merge-base": C}.get(a[0], ""))
        self.r.current_main = Mock(return_value=A)
        self.r.version_at = Mock(side_effect=lambda sha: "0.2.39" if sha == B else "0.2.38")
        self.r.script = Mock()
        self.r.finish_release = Mock(return_value=False)
        self.r.pending = Mock(return_value=pr)
        self.r.verify_version_diff = Mock(return_value=base)
        self.r.check_changes = Mock(return_value=True)
        self.r.ci = Mock(return_value=ready)
        self.r.create_version_commit = Mock(return_value=B)

    def test_create_release_pr_once_with_empty_branch_lease(self):
        self.setup_reconcile()
        def api(endpoint, method="GET", payload=None):
            if endpoint == "/user":
                return {"login": "release-bot"}
            if endpoint == "pulls":
                self.assertEqual(method, "POST")
                self.assertEqual(payload["head"], "release/v0.2.39")
                return self.pr
            self.fail(endpoint)
        with patch.object(release, "api", side_effect=api), \
                patch.object(release, "pages", side_effect=[self.published, [], []]):
            self.r.reconcile()
        self.r.git.assert_any_call("push", "--force-with-lease=refs/heads/release/v0.2.39:",
                                  "origin", f"{B}:refs/heads/release/v0.2.39")

    def test_refresh_stale_release_branch_only_with_exact_lease(self):
        self.setup_reconcile(self.pr, base=C)
        with patch.object(release, "api", return_value={"login": "release-bot"}), \
                patch.object(release, "pages", return_value=self.published):
            self.r.reconcile()
        self.r.git.assert_any_call("push", f"--force-with-lease=refs/heads/release/v0.2.39:{B}",
                                  "origin", f"{B}:refs/heads/release/v0.2.39")

    def test_ready_pr_merges_once_by_exact_sha_then_waits_for_main_ci(self):
        self.setup_reconcile(self.pr)
        live = dict(self.pr, state="open", draft=False, mergeable_state="clean")
        with patch.object(release, "api", side_effect=[{"login": "release-bot"}, live,
                                                     {"merged": True, "sha": C}]) as api, \
                patch.object(release, "pages", return_value=self.published):
            self.r.reconcile()
        self.assertEqual(api.call_args.args[:2], ("pulls/10/merge", "PUT"))
        self.assertEqual(api.call_args.args[2]["sha"], B)
        self.assertEqual(api.call_args.args[2]["merge_method"], "squash")
        self.r.script.assert_called_once()  # only prior release verification, no new tag
        self.r.create_version_commit.assert_not_called()

    def test_pending_ci_docs_only_and_main_race_never_merge(self):
        for case in ("ci", "docs", "race"):
            self.setup_reconcile(self.pr)
            if case == "ci":
                self.r.ci.return_value = False
            if case == "docs":
                self.r.check_changes.return_value = False
            if case == "race":
                self.r.current_main.side_effect = [A, C]
            with patch.object(release, "api", return_value={"login": "release-bot"}) as api, \
                    patch.object(release, "pages", return_value=self.published):
                self.r.reconcile()
            self.assertEqual(api.call_count, 1)
            self.r.create_version_commit.assert_not_called()

    def test_protection_or_changed_head_prevents_merge(self):
        for changes in ({"mergeable_state": "blocked"}, {"head": dict(self.pr["head"], sha=C)},
                        {"labels": [{"name": "release:hold"}]}, {"draft": True}):
            self.setup_reconcile(self.pr)
            live = dict(self.pr, state="open", draft=False, mergeable_state="clean")
            live.update(changes)
            with patch.object(release, "api", side_effect=[{"login": "release-bot"}, live]) as api, \
                    patch.object(release, "pages", return_value=self.published):
                if "mergeable_state" in changes:
                    with self.assertRaises(release.ReleaseError):
                        self.r.reconcile()
                else:
                    self.r.reconcile()
            self.assertEqual(api.call_count, 2)

    def test_interrupted_branch_push_is_recovered_only_for_signed_automation_commit(self):
        for verified, committer in ((True, "release-bot"), (False, "release-bot"), (True, "another-user")):
            self.setup_reconcile()
            data = {"author": {"login": "release-bot"}, "committer": {"login": committer}, "commit": {
                "verification": {"verified": verified},
                "message": f"chore(release): bump version to 0.2.39\n\n{release.MARKER}"}}
            with patch.object(release, "api", side_effect=[{"login": "release-bot"}, data, self.pr]) as api, \
                    patch.object(release, "pages", side_effect=[self.published, [], [{
                        "ref": "refs/heads/release/v0.2.39", "object": {"sha": B}}]]):
                if verified and committer == "release-bot":
                    self.r.reconcile()
                    self.assertEqual(api.call_args.args[:2], ("pulls", "POST"))
                else:
                    with self.assertRaises(release.ReleaseError):
                        self.r.reconcile()
                    self.assertEqual(api.call_count, 2)
            self.r.create_version_commit.assert_not_called()

    def test_closed_proposal_is_not_recreated_even_when_its_branch_was_deleted(self):
        self.setup_reconcile()
        with patch.object(release, "api", return_value={"login": "release-bot"}) as api, \
                patch.object(release, "pages", side_effect=[self.published, [dict(self.pr, state="closed")]]), \
                self.assertRaisesRegex(release.ReleaseError, "closed"):
            self.r.reconcile()
        self.r.create_version_commit.assert_not_called()
        self.assertEqual(api.call_count, 1)

    def test_merged_manual_release_is_rejected_before_ci_signing_or_dispatch(self):
        self.r.git = Mock(return_value=f"{C}\0chore(release): bump version to 0.2.39 (#10)")
        self.r.ci = Mock()
        self.r.script = Mock()
        for manual in (dict(self.pr, body="manual release"), dict(self.pr, user={"login": "human"})):
            with patch.object(release, "api", return_value=manual), patch.object(release, "run") as run, \
                    self.assertRaisesRegex(release.ReleaseError, "manually owned"):
                self.r.finish_release("0.2.39", self.published)
            run.assert_not_called()
        self.r.ci.assert_not_called()
        self.r.script.assert_not_called()

    def test_release_signs_only_checked_merge_commit_and_dispatches_once(self):
        self.r.git = Mock(return_value=f"{A}\0fix: newer dependency edit\n{C}\0chore(release): bump version to 0.2.39 (#10)")
        self.r.version_at = Mock(return_value="0.2.39")
        self.r.verify_version_diff = Mock(return_value=A)
        self.r.ci = Mock(return_value=True)
        self.r.script = Mock(return_value=C)
        self.r.release_runs = Mock(return_value=[])
        pr = dict(self.pr, merged=True, merge_commit_sha=C)
        with patch.object(release, "api", return_value=pr), \
                patch.object(release, "pages", return_value=[]), patch.object(release, "run") as run:
            self.assertTrue(self.r.finish_release("0.2.39", self.published))
        self.r.git.assert_any_call("-c", "tag.gpgSign=true", "tag", "-s", "v0.2.39", C, "-m", "v0.2.39")
        self.r.git.assert_any_call("verify-tag", "v0.2.39")
        self.r.script.assert_any_call("verify-release-tag.sh", "--repo", release.REPO,
                                      "--tag", "v0.2.39", "--commit", C)
        run.assert_called_once_with("gh", "workflow", "run", "release.yml", "--repo", release.REPO,
                                    "--ref", "main", "-f", "tag=v0.2.39")

    def test_rejected_tag_signature_prevents_dispatch(self):
        self.r.git = Mock(return_value=f"{C}\0chore(release): bump version to 0.2.39 (#10)")
        self.r.version_at = Mock(return_value="0.2.39")
        self.r.verify_version_diff = Mock(return_value=A)
        self.r.ci = Mock(return_value=True)
        self.r.script = Mock(side_effect=[None, release.ReleaseError("signature rejected")])
        pr = dict(self.pr, merged=True, merge_commit_sha=C)
        with patch.object(release, "api", return_value=pr), \
                patch.object(release, "pages", return_value=[{"ref": "refs/tags/v0.2.39"}]), \
                patch.object(release, "run") as run, self.assertRaises(release.ReleaseError):
            self.r.finish_release("0.2.39", self.published)
        run.assert_not_called()

    def test_post_merge_pending_ci_cannot_sign(self):
        self.r.git = Mock(return_value=f"{A}\0fix: update dependencies\n{C}\0chore(release): bump version to 0.2.39 (#10)")
        self.r.version_at = Mock(return_value="0.2.39")
        self.r.verify_version_diff = Mock(return_value=A)
        self.r.ci = Mock(return_value=False)
        pr = dict(self.pr, merged=True, merge_commit_sha=C)
        with patch.object(release, "api", return_value=pr), patch.object(release, "run") as run:
            self.assertTrue(self.r.finish_release("0.2.39", self.published))
            run.assert_not_called()
        self.assertFalse(any(call.args[0] == "push" for call in self.r.git.call_args_list))

    def test_existing_signed_tag_active_and_failed_builds_never_redispatch(self):
        for status in ("in_progress", "completed"):
            self.r.git = Mock(return_value=f"{C}\0chore(release): bump version to 0.2.39 (#10)")
            self.r.version_at = Mock(return_value="0.2.39")
            self.r.verify_version_diff = Mock(return_value=A)
            self.r.ci = Mock(return_value=True)
            self.r.script = Mock(return_value=C)
            self.r.release_runs = Mock(return_value=[{"status": status, "html_url": "run/42"}])
            pr = dict(self.pr, merged=True, merge_commit_sha=C)
            with patch.object(release, "api", return_value=pr), \
                    patch.object(release, "pages", return_value=[{"ref": "refs/tags/v0.2.39"}]), \
                    patch.object(release, "run") as run:
                if status == "completed":
                    with self.assertRaises(release.ReleaseError):
                        self.r.finish_release("0.2.39", self.published)
                else:
                    self.r.finish_release("0.2.39", self.published)
                run.assert_not_called()
            self.assertFalse(any(call.args[0] == "push" for call in self.r.git.call_args_list))


if __name__ == "__main__":
    unittest.main()
