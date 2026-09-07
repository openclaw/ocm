import importlib.util
import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import HTTPError

SPEC = importlib.util.spec_from_file_location(
    "npm_release", Path(__file__).parents[1] / "npm_release.py"
)
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


def fixture_assets(directory, binary=b"native bytes"):
    directory.mkdir()
    for target in release.TARGETS.values():
        with tarfile.open(directory / f"ocm-{target}.tar.gz", "w:gz") as tar:
            for name, data in {
                "ocm": binary,
                "LICENSE": b"license",
                "README.md": b"readme",
            }.items():
                info = tarfile.TarInfo(name)
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))


def stage_fixture(root, version="1.0.0", binary=b"native bytes"):
    assets = root / f"assets-{version}"
    fixture_assets(assets, binary)
    output = root / f"npm-{version}"
    receipt = release.stage(
        version,
        {"tag": f"v{version}", "commit": "1" * 40},
        assets,
        (release.ROOT / "npm/ocm.cjs").read_bytes(),
        b"license",
        b"readme",
        output,
    )
    return output, receipt


class NpmReleaseTests(unittest.TestCase):
    def test_original_release_checksums_are_verified_not_regenerated(self):
        for fault in [
            "none",
            "github-digest",
            "original-checksum",
            "duplicate-checksum",
        ]:
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as temporary:
                files = {
                    f"ocm-{target}.tar.gz": b"archive"
                    for target in release.TARGETS.values()
                }
                files["install.sh"] = b"installer"
                checksums = "".join(
                    f"{hashlib.sha256(data).hexdigest()}  {name}\n"
                    for name, data in files.items()
                )
                if fault == "original-checksum":
                    checksums = "0" * 64 + checksums[64:]
                elif fault == "duplicate-checksum":
                    checksums += checksums.splitlines()[0] + "\n"
                files["SHA256SUMS"] = checksums.encode()
                snapshot = {
                    name: {
                        "size": len(data),
                        "digest": "sha256:" + hashlib.sha256(data).hexdigest(),
                    }
                    for name, data in files.items()
                }
                if fault == "github-digest":
                    snapshot["install.sh"]["digest"] = "sha256:" + "0" * 64

                def download(url, **_):
                    return io.BytesIO(files[url.rsplit("/", 1)[-1]])

                with patch.object(release, "urlopen", side_effect=download):
                    if fault == "none":
                        release.download_assets(
                            "openclaw/ocm", "v1.0.0", snapshot, Path(temporary)
                        )
                        self.assertEqual(
                            (Path(temporary) / "SHA256SUMS").read_bytes(),
                            files["SHA256SUMS"],
                        )
                    else:
                        with self.assertRaises(ValueError):
                            release.download_assets(
                                "openclaw/ocm", "v1.0.0", snapshot, Path(temporary)
                            )

    def test_reserved_versions_and_semver_order(self):
        for version in [
            "1.0.0+build",
            "1.0.0-darwin-arm64",
            "1.0.0-rc.1-linux-x64",
            "1.0.0-01",
            "01.0.0",
        ]:
            with self.subTest(version=version), self.assertRaises(ValueError):
                release.validate_version(version)
        versions = ["1.0.0-alpha", "1.0.0-alpha.2", "1.0.0-alpha.10", "1.0.0", "1.0.1"]
        self.assertEqual(sorted(reversed(versions), key=release.version_key), versions)

    def test_packages_are_deterministic_and_alias_exact_native_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output, receipt = stage_fixture(root)
            second = root / "second"
            second.mkdir()
            other, again = stage_fixture(second)
            self.assertEqual(receipt, again)
            for entry in receipt["packages"]:
                self.assertEqual(
                    (output / entry["file"]).read_bytes(),
                    (other / entry["file"]).read_bytes(),
                )
                with tarfile.open(output / entry["file"]) as tar:
                    manifest = json.load(tar.extractfile("package/package.json"))
                    self.assertEqual(manifest["name"], "@openclaw/ocm")
                    self.assertNotIn("scripts", manifest)
                    if entry["version"] == "1.0.0":
                        self.assertEqual(
                            manifest["optionalDependencies"]["@openclaw/ocm-linux-x64"],
                            "npm:@openclaw/ocm@1.0.0-linux-x64",
                        )
                        self.assertEqual(manifest["bin"], {"ocm": "bin/ocm.cjs"})
                    else:
                        platform = entry["version"].removeprefix("1.0.0-")
                        native = tar.extractfile(
                            f"package/vendor/{release.TARGETS[platform]}/bin/ocm"
                        ).read()
                        self.assertEqual(native, b"native bytes")
                        self.assertNotIn("bin", manifest)
                        if platform == "linux-x64":
                            self.assertEqual(manifest["libc"], ["glibc"])

    def test_native_archive_rejects_links_extra_entries_and_traversal(self):
        with tempfile.TemporaryDirectory() as temporary:
            archive = Path(temporary) / "bad.tar.gz"
            for bad_name, kind in [
                ("../ocm", tarfile.REGTYPE),
                ("ocm", tarfile.SYMTYPE),
                ("extra", tarfile.REGTYPE),
            ]:
                with self.subTest(bad_name=bad_name, kind=kind):
                    with tarfile.open(archive, "w:gz") as tar:
                        for name in ["LICENSE", "README.md", bad_name]:
                            info = tarfile.TarInfo(name)
                            info.type = kind if name == bad_name else tarfile.REGTYPE
                            info.linkname = (
                                "elsewhere" if kind == tarfile.SYMTYPE else ""
                            )
                            tar.addfile(info)
                    with self.assertRaises(ValueError):
                        release.native_bytes(archive)

    def test_registry_only_treats_404_as_absent(self):
        for code in [401, 403, 429, 500, 404]:
            error = HTTPError(
                "https://registry.npmjs.org/test", code, "failure", {}, None
            )
            with patch.object(release, "urlopen", side_effect=error):
                if code == 404:
                    self.assertIsNone(release.registry_json("test"))
                else:
                    with self.assertRaises(HTTPError):
                        release.registry_json("test")
        for body in [b"null", b"[]", b"42", b'"text"']:
            with (
                self.subTest(body=body),
                patch.object(release, "urlopen", return_value=io.BytesIO(body)),
            ):
                with self.assertRaisesRegex(ValueError, "non-object"):
                    release.registry_json("test")

    def test_partial_retry_publishes_missing_payloads_then_root(self):
        with tempfile.TemporaryDirectory() as temporary:
            output, receipt = stage_fixture(Path(temporary))
            first = receipt["packages"][0]

            def registry(path):
                if path.endswith(first["version"]):
                    return {"dist": {"integrity": first["integrity"]}}
                return None

            with (
                patch.object(release, "registry_json", side_effect=registry),
                patch.object(release.subprocess, "run") as publish,
            ):
                release.publish(output)
                files = [Path(call.args[0][2]).name for call in publish.call_args_list]
                self.assertEqual(
                    files, [entry["file"] for entry in receipt["packages"][1:]]
                )
                self.assertEqual(publish.call_args_list[-1].args[0][-1], "latest")

    def test_existing_integrity_mismatch_and_tag_rollback_fail_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            output, receipt = stage_fixture(Path(temporary))
            cases = ["integrity", "rollback", "repair", "complete"]
            for case in cases:

                def registry(path):
                    if path.endswith("/dist-tags"):
                        return {
                            "latest": "2.0.0"
                            if case in ("rollback", "complete")
                            else "0.9.0"
                        }
                    entry = next(
                        entry
                        for entry in receipt["packages"]
                        if path.endswith("/" + entry["version"])
                    )
                    if case == "integrity":
                        return {"dist": {"integrity": "sha512-different"}}
                    if case == "rollback" and entry["version"] == "1.0.0":
                        return None
                    return {"dist": {"integrity": entry["integrity"]}}

                with (
                    self.subTest(case=case),
                    patch.object(release, "registry_json", side_effect=registry),
                    patch.object(release.subprocess, "run") as publish,
                ):
                    if case == "complete":
                        release.publish(output)
                    else:
                        with self.assertRaises(ValueError):
                            release.publish(output)
                    publish.assert_not_called()

    def test_tampered_prepared_tarball_is_never_published(self):
        with tempfile.TemporaryDirectory() as temporary:
            output, receipt = stage_fixture(Path(temporary))
            (output / receipt["packages"][0]["file"]).write_bytes(b"tampered")
            with (
                patch.object(release.subprocess, "run") as publish,
                self.assertRaises(ValueError),
            ):
                release.publish(output)
            publish.assert_not_called()

    def test_stale_root_or_platform_channel_is_rejected_before_any_write(self):
        with tempfile.TemporaryDirectory() as temporary:
            output, _ = stage_fixture(Path(temporary))
            for tags in [
                {"latest": "2.0.0"},
                {"platform-latest-linux-x64": "2.0.0-linux-x64"},
                {"latest": None},
            ]:
                with (
                    self.subTest(tags=tags),
                    patch.object(
                        release,
                        "registry_json",
                        side_effect=lambda path: tags
                        if path.endswith("/dist-tags")
                        else None,
                    ),
                    patch.object(release.subprocess, "run") as publish,
                ):
                    with self.assertRaises(ValueError):
                        release.publish(output)
                    publish.assert_not_called()

    def test_newer_next_does_not_block_stable_and_root_is_rechecked(self):
        with tempfile.TemporaryDirectory() as temporary:
            output, receipt = stage_fixture(Path(temporary))
            tags = {
                "next": "2.0.0-rc.1",
                "platform-next-linux-x64": "2.0.0-rc.1-linux-x64",
            }
            for move_latest in [False, True]:
                calls = []

                def registry(path):
                    if path.endswith("/dist-tags"):
                        return {
                            **tags,
                            **(
                                {"latest": "2.0.0"}
                                if move_latest and len(calls) == 3
                                else {}
                            ),
                        }
                    return None

                with (
                    self.subTest(move_latest=move_latest),
                    patch.object(release, "registry_json", side_effect=registry),
                    patch.object(
                        release.subprocess,
                        "run",
                        side_effect=lambda command, **_: calls.append(command),
                    ),
                ):
                    if move_latest:
                        with self.assertRaisesRegex(
                            ValueError, "refusing to move latest"
                        ):
                            release.publish(output)
                        self.assertEqual(len(calls), 3)
                    else:
                        release.publish(output)
                        self.assertEqual(
                            [call[-1] for call in calls],
                            [entry["tag"] for entry in receipt["packages"]],
                        )


if __name__ == "__main__":
    unittest.main()
