#!/usr/bin/env python3
"""Prepare deterministic npm packages from verified, already published binaries."""

import argparse
import base64
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
from urllib.error import HTTPError
from urllib.parse import quote
from urllib.request import urlopen

ROOT = Path(__file__).resolve().parent.parent
PACKAGE = "@openclaw/ocm"
REGISTRY = "https://registry.npmjs.org"
TARGETS = {
    "darwin-arm64": "aarch64-apple-darwin",
    "darwin-x64": "x86_64-apple-darwin",
    "linux-x64": "x86_64-unknown-linux-gnu",
}
SEMVER = re.compile(
    r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
)
MAX_ASSET = 512 * 1024 * 1024


def version_key(version):
    match = SEMVER.fullmatch(version)
    if not match:
        raise ValueError(f"invalid npm release version: {version}")
    major, minor, patch, prerelease = match.groups()
    parts = []
    for part in prerelease.split(".") if prerelease else []:
        if part.isdecimal() and len(part) > 1 and part.startswith("0"):
            raise ValueError(
                "numeric prerelease identifiers cannot have leading zeroes"
            )
        parts.append((0, int(part)) if part.isdecimal() else (1, part))
    return (int(major), int(minor), int(patch), prerelease is None, tuple(parts))


def validate_version(version):
    version_key(version)
    if any(version.endswith(f"-{platform}") for platform in TARGETS):
        raise ValueError("release version uses the reserved npm platform namespace")
    return version


def json_bytes(value):
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def integrity(data):
    return "sha512-" + base64.b64encode(hashlib.sha512(data).digest()).decode()


def github(*args):
    command = ["ghx", "--no-cache"] if shutil.which("ghx") else ["gh"]
    return subprocess.check_output([*command, *args])


def git_source(commit, filename):
    return subprocess.check_output(
        ["git", "-C", str(ROOT), "show", f"{commit}:{filename}"]
    )


def release_snapshot(repo, tag):
    release = json.loads(github("api", f"repos/{repo}/releases/tags/{tag}"))
    expected = {f"ocm-{target}.tar.gz" for target in TARGETS.values()} | {
        "install.sh",
        "SHA256SUMS",
    }
    assets = release["assets"]
    if (
        release["tag_name"] != tag
        or release["draft"]
        or not release["published_at"]
        or {asset["name"] for asset in assets} != expected
        or len(assets) != len(expected)
    ):
        raise ValueError("a complete, published binary release is required before npm")
    snapshot = {}
    for asset in assets:
        if (
            not re.fullmatch(r"sha256:[0-9a-f]{64}", asset.get("digest") or "")
            or not 0 < asset["size"] <= MAX_ASSET
            or asset["state"] != "uploaded"
        ):
            raise ValueError(f"invalid release asset metadata: {asset['name']}")
        snapshot[asset["name"]] = {
            key: asset[key] for key in ("id", "digest", "size", "updated_at")
        }
    return snapshot


def verify_source(repo, tag, expected=None):
    if repo != "openclaw/ocm":
        raise ValueError("npm publication requires openclaw/ocm")
    args = [str(ROOT / "scripts/verify-published-release.sh"), repo, tag]
    if expected:
        args.append(expected)
    commit = subprocess.check_output(args, text=True).strip()
    version = validate_version(
        tomllib.loads(git_source(commit, "Cargo.toml").decode())["package"]["version"]
    )
    if tag != f"v{version}":
        raise ValueError("release tag and npm version differ")
    # Older releases lack both the wrapper and the native package-owner guard.
    # Never combine a new wrapper with an old unprotected Rust executable.
    git_source(commit, "npm/ocm.cjs")
    git_source(commit, "src/infra/install_owner.rs")
    return commit, version


def download_assets(repo, tag, snapshot, directory):
    for name, metadata in snapshot.items():
        url = (
            f"https://github.com/{repo}/releases/download/{quote(tag, safe='')}/{name}"
        )
        with urlopen(url, timeout=60) as response:
            data = response.read(MAX_ASSET + 1)
        if (
            len(data) != metadata["size"]
            or "sha256:" + hashlib.sha256(data).hexdigest() != metadata["digest"]
        ):
            raise ValueError(f"GitHub digest or size mismatch: {name}")
        (directory / name).write_bytes(data)
    checksums = {}
    for line in (directory / "SHA256SUMS").read_text().splitlines():
        match = re.fullmatch(r"([0-9a-fA-F]{64}) [ *](\S+)", line)
        if not match or match[2] in checksums:
            raise ValueError("invalid or duplicate original SHA256SUMS entry")
        checksums[match[2]] = match[1].lower()
    if set(checksums) != set(snapshot) - {"SHA256SUMS"}:
        raise ValueError("original SHA256SUMS does not cover the exact release")
    for name, digest in checksums.items():
        if hashlib.sha256((directory / name).read_bytes()).hexdigest() != digest:
            raise ValueError(f"original SHA256SUMS mismatch: {name}")


def native_bytes(archive):
    with tarfile.open(archive, "r:gz") as tar:
        members = tar.getmembers()
        if (
            len(members) != 3
            or {member.name for member in members} != {"ocm", "LICENSE", "README.md"}
            or any(not member.isfile() or member.size > MAX_ASSET for member in members)
        ):
            raise ValueError(
                "release archive must contain only regular ocm, LICENSE and README.md files"
            )
        return tar.extractfile("ocm").read()


def pack(path, files):
    # Normalized metadata makes retries byte-identical across runners and dates.
    with (
        path.open("wb") as output,
        gzip.GzipFile(fileobj=output, mode="wb", filename="", mtime=0) as zipped,
    ):
        with tarfile.open(fileobj=zipped, mode="w", format=tarfile.USTAR_FORMAT) as tar:
            for name, (data, mode) in sorted(files.items()):
                info = tarfile.TarInfo(f"package/{name}")
                info.size = len(data)
                info.mode = mode
                tar.addfile(info, io.BytesIO(data))
    return integrity(path.read_bytes())


def stage(version, source, assets, wrapper, license_text, readme, output):
    validate_version(version)
    channel = "next" if "-" in version else "latest"
    output.mkdir(parents=True, exist_ok=False)
    common = {
        "name": PACKAGE,
        "description": "Manage OpenClaw environments, runtimes, and services",
        "license": "MIT",
        "repository": {"type": "git", "url": "git+https://github.com/openclaw/ocm.git"},
        "engines": {"node": "^22.15.0 || >=24.0.0"},
    }
    packages = []
    for platform, target in TARGETS.items():
        archive = assets / f"ocm-{target}.tar.gz"
        binary = native_bytes(archive)
        manifest = {
            **common,
            "version": f"{version}-{platform}",
            "os": [platform.split("-")[0]],
            "cpu": [platform.split("-")[1]],
        }
        if platform == "linux-x64":
            manifest["libc"] = ["glibc"]
        metadata = {
            **source,
            "target": target,
            "binarySha256": hashlib.sha256(binary).hexdigest(),
        }
        filename = f"ocm-{version}-{platform}.tgz"
        digest = pack(
            output / filename,
            {
                "package.json": (json_bytes(manifest), 0o644),
                "release.json": (json_bytes(metadata), 0o644),
                "LICENSE": (license_text, 0o644),
                f"vendor/{target}/bin/ocm": (binary, 0o755),
            },
        )
        packages.append(
            {
                "file": filename,
                "version": manifest["version"],
                "tag": f"platform-{channel}-{platform}",
                "integrity": digest,
            }
        )
    manifest = {
        **common,
        "version": version,
        "bin": {"ocm": "bin/ocm.cjs"},
        "optionalDependencies": {
            f"{PACKAGE}-{platform}": f"npm:{PACKAGE}@{version}-{platform}"
            for platform in TARGETS
        },
    }
    filename = f"ocm-{version}.tgz"
    digest = pack(
        output / filename,
        {
            "package.json": (json_bytes(manifest), 0o644),
            "release.json": (json_bytes(source), 0o644),
            "LICENSE": (license_text, 0o644),
            "README.md": (readme, 0o644),
            "bin/ocm.cjs": (wrapper, 0o755),
        },
    )
    packages.append(
        {"file": filename, "version": version, "tag": channel, "integrity": digest}
    )
    receipt = {"version": version, "source": source, "packages": packages}
    (output / "release.json").write_bytes(json_bytes(receipt))
    return receipt


def prepare(repo, tag, output):
    commit, version = verify_source(repo, tag)
    snapshot = release_snapshot(repo, tag)
    source = {"repository": repo, "tag": tag, "commit": commit, "assets": snapshot}
    with tempfile.TemporaryDirectory(prefix="ocm-npm-assets-") as temporary:
        assets = Path(temporary)
        download_assets(repo, tag, snapshot, assets)
        receipt = stage(
            version,
            source,
            assets,
            git_source(commit, "npm/ocm.cjs"),
            git_source(commit, "LICENSE"),
            git_source(commit, "npm/README.md"),
            output,
        )
    if release_snapshot(repo, tag) != snapshot:
        raise ValueError("release assets changed during packaging")
    verify_source(repo, tag, commit)
    return receipt


def read_receipt(directory):
    receipt = json.loads((directory / "release.json").read_bytes())
    version = validate_version(receipt["version"])
    expected_versions = [f"{version}-{platform}" for platform in TARGETS] + [version]
    if [entry["version"] for entry in receipt["packages"]] != expected_versions:
        raise ValueError("incomplete or reordered npm package set")
    for entry in receipt["packages"]:
        filename = entry["file"]
        if Path(filename).name != filename or not filename.endswith(".tgz"):
            raise ValueError("invalid npm tarball path")
        if integrity((directory / filename).read_bytes()) != entry["integrity"]:
            raise ValueError(f"prepared tarball integrity mismatch: {filename}")
    return receipt


def revalidate(directory, repo, tag, commit):
    receipt = read_receipt(directory)
    verified, version = verify_source(repo, tag, commit)
    source = {
        "repository": repo,
        "tag": tag,
        "commit": verified,
        "assets": release_snapshot(repo, tag),
    }
    if receipt["version"] != version or receipt["source"] != source:
        raise ValueError("release source or assets changed after npm validation")


def registry_json(path):
    try:
        with urlopen(f"{REGISTRY}/{path}", timeout=30) as response:
            data = response.read(2 * 1024 * 1024 + 1)
    except HTTPError as error:
        if error.code == 404:
            return None
        raise
    if len(data) > 2 * 1024 * 1024:
        raise ValueError("npm registry response is too large")
    value = json.loads(data)
    if not isinstance(value, dict):
        raise ValueError("npm registry returned a non-object JSON response")
    return value


def existing_version(entry):
    existing = registry_json(
        f"{quote(PACKAGE, safe='')}/{quote(entry['version'], safe='')}"
    )
    if existing is None:
        return False
    if existing.get("dist", {}).get("integrity") != entry["integrity"]:
        raise ValueError(
            f"npm already has different bytes for {PACKAGE}@{entry['version']}"
        )
    return True


def require_forward_tag(tags, tag, version, platform=None):
    if tag not in tags:
        return
    current = tags[tag]
    if not isinstance(current, str):
        raise ValueError(f"npm returned an invalid version for {tag}")
    if platform:
        suffix = f"-{platform}"
        if not current.endswith(suffix):
            raise ValueError(f"npm tag {tag} points outside its platform namespace")
        current = current[: -len(suffix)]
    if version_key(current) >= version_key(version):
        raise ValueError(f"refusing to move {tag} backward or replace its version")


def publish(directory):
    receipt = read_receipt(directory)
    version = receipt["version"]
    channel = "next" if "-" in version else "latest"
    if os.environ.get("NODE_AUTH_TOKEN") or os.environ.get("NPM_TOKEN"):
        raise ValueError(
            "token fallback is forbidden; use the configured npm trusted publisher"
        )
    entries = receipt["packages"]
    present = [existing_version(entry) for entry in entries]
    tags_path = f"-/package/{quote(PACKAGE, safe='')}/dist-tags"
    tags = registry_json(tags_path) or {}
    if present[-1]:
        current = tags.get(channel)
        if (
            all(present)
            and isinstance(current, str)
            and version_key(current) >= version_key(version)
        ):
            print(
                f"Verified completed {PACKAGE}@{version}; {channel} stays at {current}"
            )
            return
        raise ValueError(
            f"{PACKAGE}@{version} exists but needs explicit maintainer package/tag recovery"
        )

    channels = [
        (f"platform-{channel}-{platform}", platform) for platform in TARGETS
    ] + [(channel, None)]
    # Preflight the entire transaction before any registry write. Separate stable
    # and prerelease platform tags so a newer `next` cannot block a stable release.
    for exists, (tag, platform) in zip(present, channels):
        if not exists:
            require_forward_tag(tags, tag, version, platform)
    for entry, exists, (tag, platform) in zip(entries, present, channels):
        if exists:
            print(f"Verified existing {PACKAGE}@{entry['version']}")
            continue
        # Explicit --tag disables npm's older-version safeguard. Recheck each
        # tag immediately before publication, including the root channel last.
        require_forward_tag(registry_json(tags_path) or {}, tag, version, platform)
        subprocess.run(
            [
                "npm",
                "publish",
                str(directory / entry["file"]),
                "--access",
                "public",
                "--ignore-scripts",
                "--provenance",
                "--registry",
                REGISTRY,
                "--tag",
                tag,
            ],
            check=True,
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    prepare_parser = sub.add_parser("prepare")
    prepare_parser.add_argument("repo")
    prepare_parser.add_argument("tag")
    prepare_parser.add_argument("output", type=Path)
    verify_parser = sub.add_parser("verify")
    verify_parser.add_argument("directory", type=Path)
    verify_parser.add_argument("repo")
    verify_parser.add_argument("tag")
    verify_parser.add_argument("commit")
    publish_parser = sub.add_parser("publish")
    publish_parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    if args.command == "prepare":
        receipt = prepare(args.repo, args.tag, args.output)
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a") as output:
                output.write(f"source={receipt['source']['commit']}\n")
    elif args.command == "verify":
        revalidate(args.directory, args.repo, args.tag, args.commit)
    else:
        publish(args.directory)


if __name__ == "__main__":
    main()
