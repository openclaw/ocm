"""Real npm installs against a task-local, read-only fixture registry."""

from contextlib import contextmanager
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import sys
import tarfile
import tempfile
from threading import Thread
import unittest
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


def run(command, cwd, env, **kwargs):
    result = subprocess.run(
        command, cwd=cwd, env=env, text=True, capture_output=True, timeout=90, **kwargs
    )
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


class NpmInstallTests(unittest.TestCase):
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


def smoke(directory):
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
            if (
                hashlib.sha256(binary.read_bytes()).hexdigest()
                != metadata["binarySha256"]
            ):
                raise AssertionError("npm install changed native executable bytes")
            entrypoint = prefix / "bin/ocm"
            result = run([str(entrypoint), "--version"], root, env)
            if receipt["version"] not in result.stdout:
                raise AssertionError(
                    "installed executable version differs from package"
                )
            run([str(entrypoint), "--help"], root, env)
            if sys.platform == "darwin":
                run(
                    [
                        str(release.ROOT / "scripts/verify-macos-release.sh"),
                        "--binary",
                        str(binary),
                        "--team-id",
                        os.environ["MACOS_TEAM_ID"],
                        "--require-notarization",
                    ],
                    root,
                    env,
                )
            print(
                f"Verified npm install, CLI, and unchanged native bytes on {platform.platform()}"
            )


def published_binary_fixture():
    # This old release proves signed-byte preservation only. Production prepare
    # rejects it because the native npm ownership guard had not shipped.
    repo, tag = "openclaw/ocm", "v0.2.39"
    snapshot = release.release_snapshot(repo, tag)
    with tempfile.TemporaryDirectory(prefix="ocm-npm-release-fixture-") as temporary:
        root = Path(temporary)
        assets = root / "assets"
        assets.mkdir()
        release.download_assets(repo, tag, snapshot, assets)
        output = root / "npm"
        release.stage(
            "0.2.39",
            {"repository": repo, "tag": tag, "assets": snapshot},
            assets,
            (release.ROOT / "npm/ocm.cjs").read_bytes(),
            (release.ROOT / "LICENSE").read_bytes(),
            (release.ROOT / "npm/README.md").read_bytes(),
            output,
        )
        smoke(output)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--packages":
        smoke(Path(sys.argv[2]).resolve())
    elif sys.argv[1:] == ["--published-binary-fixture"]:
        published_binary_fixture()
    else:
        unittest.main()
