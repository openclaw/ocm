#!/usr/bin/env python3
"""One bounded, restartable reconciliation of OCM's existing release protocol.

Run only from a disposable checkout with a configured Git signer and gh identity.
Publication stays exclusively with release.yml and its existing verifiers.
"""

import argparse
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = "openclaw/ocm"
ROOT = Path(__file__).resolve().parent.parent
VERSION = re.compile(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)\Z")
SHA = re.compile(r"[0-9a-f]{40}\Z")
MARKER = "<!-- ocm-auto-release:v1 -->"
CI_JOBS = {"Format", "Rust 1.88 minimum", "Windows compile",
           "Test (ubuntu-latest)", "Test (macos-latest)",
           "npm (ubuntu-latest, Node 22.15.0)", "npm (ubuntu-latest, Node 24)",
           "npm (macos-15-intel, Node 24)", "npm (macos-15, Node 24)"}


class ReleaseError(RuntimeError):
    pass


def run(*args, cwd=None, data=None, strip=True):
    cwd = ROOT if cwd is None else cwd
    result = subprocess.run(args, cwd=cwd, input=data, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=180)
    if result.returncode:
        # Do not echo argv or credential-bearing authentication diagnostics.
        raise ReleaseError(f"{args[0]} {args[1] if len(args) > 1 else ''} failed ({result.returncode})")
    return result.stdout.strip() if strip else result.stdout


def api(endpoint, method="GET", payload=None):
    args = ["gh", "api", f"repos/{REPO}/{endpoint}" if endpoint != "/user" else "user",
            "--method", method]
    if payload is not None:
        args += ["--input", "-"]
    result = run(*args, data=json.dumps(payload) if payload is not None else None)
    return json.loads(result) if result else None


def pages(endpoint):
    result = []
    for page in range(1, 101):
        batch = api(f"{endpoint}{'&' if '?' in endpoint else '?'}per_page=100&page={page}")
        if not isinstance(batch, list):
            raise ReleaseError("Malformed paginated GitHub response")
        result += batch
        if len(batch) < 100:
            return result
    raise ReleaseError("GitHub pagination limit exceeded")


def version_tuple(version):
    if not VERSION.fullmatch(version):
        raise ReleaseError(f"Automatic releases require a stable version: {version}")
    return tuple(map(int, version.split(".")))


def next_version(version, bump):
    major, minor, patch = version_tuple(version)
    if bump == "major":
        return f"{major + 1}.0.0"
    if bump == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def docs_only(paths):
    # Conservative allowlist. Skills, scripts, CI, installers and unknown files
    # are releasable even when they look like documentation.
    return bool(paths) and all(
        p.startswith("docs/") or p in {"README.md", "CONTRIBUTING.md", "CHANGELOG.md"}
        for p in paths)


def needs_explicit_bump(messages):
    return any(re.search(r"(?m)^BREAKING[ -]CHANGE:|^[^\n:]+!:", m) for m in messages)


def checked_sha(value):
    if not isinstance(value, str) or not SHA.fullmatch(value):
        raise ReleaseError("Invalid commit identity")
    return value


class Reconciler:
    def __init__(self, bump="auto"):
        self.bump = bump
        self.main = None
        self.actor = None

    def git(self, *args):
        return run("git", *args, strip=args[0] != "show")

    def script(self, name, *args):
        return run(str(ROOT / "scripts" / name), *args)

    def current_main(self):
        return checked_sha(api("git/ref/heads/main")["object"]["sha"])

    def version_at(self, commit):
        with tempfile.TemporaryDirectory(prefix="ocm-auto-version-") as directory:
            files = []
            for name in ("Cargo.toml", "Cargo.lock"):
                path = Path(directory) / name
                path.write_text(self.git("show", f"{commit}:{name}"))
                files.append(str(path))
            return self.script("read-package-version.sh", *files)

    def ci(self, commit, branch, event):
        data = api(f"actions/workflows/ci.yml/runs?head_sha={commit}&event={event}&per_page=100")
        runs = [r for r in data["workflow_runs"]
                if r["head_sha"] == commit and r["head_branch"] == branch
                and r["event"] == event and r["head_repository"]["full_name"] == REPO]
        if len(runs) > 1:
            raise ReleaseError("Ambiguous CI runs; rerun the existing run rather than creating another")
        if not runs or runs[0]["status"] != "completed":
            return False
        ci_run = runs[0]
        if ci_run["conclusion"] != "success":
            raise ReleaseError(f"CI did not pass: {ci_run['html_url']}")
        jobs = api(f"actions/runs/{ci_run['id']}/jobs?filter=latest&per_page=100")
        actual = jobs["jobs"]
        if jobs["total_count"] != len(actual) or len(actual) != len(CI_JOBS) or \
                {j["name"] for j in actual} != CI_JOBS or \
                any(j["conclusion"] != "success" for j in actual):
            raise ReleaseError("CI did not complete every required release job successfully")
        return True

    def release_runs(self, tag):
        # run-name in release.yml is exactly "Release <tag>". Inspect all pages:
        # a prior dispatch must not disappear just because other releases ran.
        runs = []
        for page in range(1, 101):
            response = api(f"actions/workflows/release.yml/runs?event=workflow_dispatch&per_page=100&page={page}")
            batch = response["workflow_runs"]
            runs += [r for r in batch if r["display_title"] == f"Release {tag}"
                     and r["head_branch"] == "main"
                     and r["head_repository"]["full_name"] == REPO]
            if len(batch) < 100:
                return runs
        raise ReleaseError("Release run pagination limit exceeded")

    def finish_release(self, version, releases):
        tag = f"v{version}"
        published = [r for r in releases if r["tag_name"] == tag and not r["draft"]
                     and not r["prerelease"]]
        if published:
            return False
        # Later dependency edits may also touch Cargo.toml. Find the canonical
        # version-introducing release commit, not simply its last file edit.
        title = f"chore(release): bump version to {version}"
        candidates = []
        for line in self.git("log", "--first-parent", "--format=%H%x00%s", self.main,
                             "--", "Cargo.toml").splitlines():
            sha, subject = line.split("\0", 1)
            match = re.fullmatch(re.escape(title) + r" \(#([1-9]\d*)\)", subject)
            if match:
                candidates.append((checked_sha(sha), match[1]))
        if len(candidates) != 1:
            raise ReleaseError("Unpublished version is not a canonical merged release PR")
        commit, number = candidates[0]
        pr = api(f"pulls/{number}")
        if pr["user"]["login"] != self.actor or MARKER not in (pr["body"] or ""):
            raise ReleaseError("Unpublished release is manually owned; leaving signing and dispatch to its owner")
        if not pr["merged"] or pr["base"]["ref"] != "main" or \
                pr["head"]["ref"] != f"release/{tag}" or \
                pr["merge_commit_sha"] != commit or pr["title"] != title or \
                self.version_at(commit) != version:
            raise ReleaseError("Release PR does not match the unpublished version")
        self.verify_version_diff(commit, version)
        if not self.ci(commit, "main", "push"):
            print(f"Waiting for post-merge CI on {commit}")
            return True
        self.script("verify-release-ci.sh", "--repo", REPO, "--commit", commit)
        refs = pages("git/matching-refs/tags/" + tag)
        refs = [r for r in refs if r["ref"] == "refs/tags/" + tag]
        if not refs:
            # Never move/recreate an existing tag. Sign the checked release SHA,
            # not HEAD or a moving branch. Authentication is gh's credential helper.
            self.git("-c", "tag.gpgSign=true", "tag", "-s", tag, commit, "-m", tag)
            self.git("verify-tag", tag)
            self.git("push", "origin", "refs/tags/" + tag)
        self.script("verify-release-tag.sh", "--repo", REPO, "--tag", tag, "--commit", commit)
        runs = self.release_runs(tag)
        if runs:
            latest = runs[0]
            if latest["status"] == "completed":
                raise ReleaseError(f"Release has no published result; inspect or rerun {latest['html_url']}")
            print(f"Release build already running for {tag}")
        else:
            run("gh", "workflow", "run", "release.yml", "--repo", REPO,
                "--ref", "main", "-f", f"tag={tag}")
            print(f"Dispatched existing signed-release workflow for {tag}")
        return True

    def verify_version_diff(self, commit, version):
        parents = self.git("rev-list", "--parents", "-n", "1", commit).split()
        if len(parents) != 2:
            raise ReleaseError("Release commit must have exactly one parent")
        parent = checked_sha(parents[1])
        paths = self.git("diff", "--name-only", parent, commit).splitlines()
        if set(paths) != {"Cargo.toml", "Cargo.lock"}:
            raise ReleaseError("Release PR must change only the two version files")
        old_version = self.version_at(parent)
        if version_tuple(version) <= version_tuple(old_version):
            raise ReleaseError("Release version must increase")
        # Verify bytes, not just filenames: no dependency or package changes may
        # be smuggled into a PR that this automation can merge.
        with tempfile.TemporaryDirectory(prefix="ocm-version-diff-") as directory:
            expected = Path(directory)
            (expected / "scripts").mkdir()
            for name in ("update-version.sh", "read-package-version.sh", "validate-version.sh"):
                target = expected / "scripts" / name
                target.write_bytes((ROOT / "scripts" / name).read_bytes())
                target.chmod(0o755)
            for name in paths:
                (expected / name).write_text(self.git("show", f"{parent}:{name}"))
            run(str(expected / "scripts/update-version.sh"), version)
            for name in paths:
                if self.git("show", f"{commit}:{name}") != (expected / name).read_text():
                    raise ReleaseError("Release PR contains changes beyond the package version")
        return parent

    def pending(self):
        prs = pages("pulls?state=open&base=main")
        candidates = [p for p in prs if p["head"]["ref"].startswith("release/")]
        if len(candidates) > 1:
            raise ReleaseError("Multiple release PRs exist; resolve ownership before releasing")
        if not candidates:
            return None
        pr = candidates[0]
        if pr["user"]["login"] != self.actor or MARKER not in (pr["body"] or "") or \
                pr["head"]["repo"]["full_name"] != REPO:
            raise ReleaseError("A manually owned release PR exists; leaving it untouched")
        return pr

    def check_changes(self, last_commit, bump):
        paths = self.git("diff", "--name-only", last_commit, self.main).splitlines()
        if not paths or docs_only(paths):
            return False
        messages = self.git("log", "--format=%B%x00", f"{last_commit}..{self.main}").split("\0")
        if needs_explicit_bump(messages) and bump not in {"minor", "major"}:
            raise ReleaseError("Declared breaking change: dispatch with an explicit minor or major bump")
        return True

    def create_version_commit(self, version):
        # Work in a private index/tree. Do not switch to or execute PR source.
        with tempfile.TemporaryDirectory(prefix="ocm-auto-release-") as directory:
            self.git("worktree", "add", "--detach", directory, self.main)
            try:
                # Run only the version editor from this trusted workflow checkout.
                # Its repo-relative root requires a copy alongside the version files.
                trusted_scripts = ROOT / "scripts"
                for name in ("update-version.sh", "read-package-version.sh", "validate-version.sh"):
                    target = Path(directory) / "scripts" / name
                    target.write_bytes((trusted_scripts / name).read_bytes())
                    target.chmod(0o755)
                run(str(Path(directory) / "scripts/update-version.sh"), version, cwd=directory)
                run("git", "add", "Cargo.toml", "Cargo.lock", cwd=directory)
                run("git", "commit", "-m", f"chore(release): bump version to {version}",
                    "-m", MARKER, cwd=directory)
                commit = checked_sha(run("git", "rev-parse", "HEAD", cwd=directory))
                self.verify_version_diff(commit, version)
                return commit
            finally:
                self.git("worktree", "remove", "--force", directory)

    def reconcile(self):
        if self.git("status", "--porcelain"):
            raise ReleaseError("Use a clean disposable checkout")
        if self.git("config", "--get", "remote.origin.url") not in {
                f"https://github.com/{REPO}", f"https://github.com/{REPO}.git", f"git@github.com:{REPO}.git"}:
            raise ReleaseError("Origin must be the canonical OCM repository")
        self.actor = api("/user")["login"]
        self.git("fetch", "origin", "main", "--tags")
        self.main = self.current_main()
        self.git("cat-file", "-e", self.main + "^{commit}")
        version = self.version_at(self.main)
        version_tuple(version)
        releases = pages("releases")
        if self.finish_release(version, releases):
            return
        release = next(r for r in releases if r["tag_name"] == f"v{version}" and not r["draft"] and not r["prerelease"])
        tag = release["tag_name"]
        last_commit = checked_sha(self.git("rev-parse", tag + "^{commit}"))
        self.script("verify-release-tag.sh", "--repo", REPO, "--tag", tag, "--commit", last_commit)
        if self.git("merge-base", last_commit, self.main) != last_commit:
            raise ReleaseError("Last release is not an ancestor of main")
        pr = self.pending()
        bump = self.bump
        if pr:
            head = checked_sha(pr["head"]["sha"])
            self.git("fetch", "origin", "refs/heads/" + pr["head"]["ref"])
            target = self.version_at(head)
            base = self.verify_version_diff(head, target)
            inferred = next((b for b in ("patch", "minor", "major") if next_version(version, b) == target), None)
            if not inferred or pr["head"]["ref"] != f"release/v{target}" or \
                    pr["title"] != f"chore(release): bump version to {target}":
                raise ReleaseError("Pending release PR has an unexpected version or identity")
            if bump != "auto" and bump != inferred:
                raise ReleaseError("Close the pending release PR before selecting a different bump")
            bump = inferred
        else:
            target = next_version(version, bump)
        if not self.check_changes(last_commit, bump):
            print("No unreleased product changes")
            return
        if not self.ci(self.main, "main", "push"):
            print("Waiting for current main CI")
            return
        if self.current_main() != self.main:
            print("Main advanced; next reconciliation will include it")
            return
        branch = f"release/v{target}"
        if not pr:
            previous = pages(f"pulls?state=closed&base=main&head=openclaw:{branch}")
            if previous:
                raise ReleaseError("A prior release proposal was closed; reopen it explicitly or select another version")
            # Recover a push that succeeded before PR creation failed. Adopt only
            # our signed, canonical version-only commit, never a manual branch.
            refs = pages("git/matching-refs/heads/" + branch)
            refs = [r for r in refs if r["ref"] == "refs/heads/" + branch]
            if refs:
                orphan = checked_sha(refs[0]["object"]["sha"])
                self.git("fetch", "origin", "refs/heads/" + branch)
                data = api(f"commits/{orphan}")
                if (data.get("author") or {}).get("login") != self.actor or \
                        (data.get("committer") or {}).get("login") != self.actor or \
                        not data["commit"]["verification"]["verified"] or \
                        data["commit"]["message"].strip() != \
                        f"chore(release): bump version to {target}\n\n{MARKER}":
                    raise ReleaseError("Existing release branch is not owned by this automation")
                self.verify_version_diff(orphan, target)
                self.ensure_pr(branch, target)
                return
        if not pr or base != self.main:
            commit = self.create_version_commit(target)
            # Empty lease means branch must not exist. Never overwrite manual work.
            expected = head if pr else ""
            self.git("push", f"--force-with-lease=refs/heads/{branch}:{expected}",
                     "origin", f"{commit}:refs/heads/{branch}")
            if not pr:
                pr = self.ensure_pr(branch, target)
            print(f"Release PR ready for CI: {pr['html_url']}")
            return
        if not self.ci(head, branch, "pull_request"):
            print(f"Waiting for release PR CI: {pr['html_url']}")
            return
        live = api(f"pulls/{pr['number']}")
        if live["head"]["sha"] != head or live["state"] != "open" or live["draft"] or \
                live["title"] != pr["title"] or MARKER not in (live["body"] or "") or \
                live["base"]["ref"] != "main" or \
                any(label["name"] == "release:hold" for label in live.get("labels", [])) or \
                self.current_main() != self.main:
            print("Release PR or main changed; leaving it for reconciliation")
            return
        if live["mergeable_state"] != "clean":
            raise ReleaseError("Release PR is not mergeable under normal branch protection")
        result = api(f"pulls/{pr['number']}/merge", "PUT", {
            "sha": head, "merge_method": "squash",
            "commit_title": f"{pr['title']} (#{pr['number']})"})
        if not result["merged"]:
            raise ReleaseError("GitHub did not confirm release PR merge")
        print(f"Merged version PR at {checked_sha(result['sha'])}; waiting for exact main CI")

    def ensure_pr(self, branch, version):
        return api("pulls", "POST", {
            "base": "main", "head": branch,
            "title": f"chore(release): bump version to {version}",
            "body": f"{MARKER}\n\nAutomatic version-only release. CI must pass before squash merge.\n"
                    "The signed-tag and complete-artifact release checks remain required."})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bump", choices=("auto", "patch", "minor", "major"), default="auto")
    args = parser.parse_args()
    try:
        Reconciler(args.bump).reconcile()
    except (ReleaseError, KeyError, ValueError, subprocess.TimeoutExpired) as error:
        print(f"Automatic release stopped: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
