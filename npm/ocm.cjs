#!/usr/bin/env node
"use strict";

const { createRequire } = require("node:module");
const { dirname, join } = require("node:path");

function main() {
  if (typeof process.execve !== "function") {
    throw new Error("OCM requires Node.js ^22.15.0 or >=24.0.0 with process.execve.");
  }
  const targets = {
    "darwin-arm64": "aarch64-apple-darwin",
    "darwin-x64": "x86_64-apple-darwin",
    "linux-x64": "x86_64-unknown-linux-gnu",
  };
  const platform = `${process.platform}-${process.arch}`;
  const target = targets[platform];
  if (!target || (process.platform === "linux" &&
      !process.report.getReport().header.glibcVersionRuntime)) {
    throw new Error(`Unsupported OCM platform: ${platform}. Supported: macOS ARM64/x64 and Linux x64 glibc.`);
  }
  const dependency = `@openclaw/ocm-${platform}`;
  let manifest;
  try {
    manifest = createRequire(__filename).resolve(`${dependency}/package.json`);
  } catch {
    throw new Error(`Missing ${dependency}. Reinstall @openclaw/ocm with optional dependencies enabled (do not use --omit=optional).`);
  }
  const binary = join(dirname(manifest), "vendor", target, "bin", "ocm");
  // OCM may escape a managed gateway's process group before stopping it.
  // There must be no Node parent left to forward that group's termination.
  process.execve(binary, [binary, ...process.argv.slice(2)], process.env);
}

try {
  main();
} catch (error) {
  console.error(`ocm: ${error.message}`);
  process.exitCode = 1;
}
