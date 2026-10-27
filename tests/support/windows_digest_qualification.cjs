'use strict';
// Temporary native cause qualification. Only the disposable checkout is marked.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const cp = require('node:child_process');
const crypto = require('node:crypto');
const readline = require('node:readline');

assert.equal(process.platform, 'win32');
const revision = process.argv[2];
assert.match(revision, /^[a-f0-9]{40}$/);
const assemblyOnly = process.argv[3] === '--assembly-only';
const expectedDiagnosticTree = process.argv[4];
if (assemblyOnly) assert.match(expectedDiagnosticTree, /^[a-f0-9]{40}$/);
const repository = process.cwd();
const root = fs.mkdtempSync(path.join(process.env.RUNNER_TEMP, 'ocm-digest-qualification-'));
const fixtureDriver = path.join(repository, 'tests/support/windows_runtime_pack.cjs');

function run(program, args, options = {}) {
  console.log(JSON.stringify({phase: 'command', program, args, cwd: options.cwd || repository}));
  const result = cp.spawnSync(program, args, {cwd: repository, stdio: 'inherit', ...options});
  if (result.error) throw result.error;
  assert.equal(result.status, 0, `${program} failed: ${result.status}`);
}
function git(args, cwd) {
  return cp.execFileSync('git', args, {cwd, encoding: 'utf8'}).trim();
}
async function sha256(file) {
  const hash = crypto.createHash('sha256');
  for await (const chunk of fs.createReadStream(file)) hash.update(chunk);
  return hash.digest('hex');
}
function stackReserve(binary) {
  const bytes = fs.readFileSync(binary);
  assert.equal(bytes.toString('ascii', 0, 2), 'MZ');
  const pe = bytes.readUInt32LE(0x3c);
  assert.equal(bytes.toString('ascii', pe, pe + 4), 'PE\0\0');
  const optional = pe + 24;
  const magic = bytes.readUInt16LE(optional);
  assert.equal(magic, 0x20b, 'Native x64 PE32+ expected');
  return bytes.readBigUInt64LE(optional + 72).toString();
}
async function identity(label, checkout, binary) {
  const result = {phase: 'identity', label,
    head: git(['rev-parse', 'HEAD'], checkout),
    tree: git(['write-tree'], checkout),
    status: git(['status', '--porcelain'], checkout),
    lockSha256: await sha256(path.join(checkout, 'Cargo.lock')),
    digestSourceSha256: await sha256(path.join(checkout, 'src/infra/tree_digest.rs')),
    binary, binarySha256: await sha256(binary), stackReserveBytes: stackReserve(binary)};
  console.log(JSON.stringify(result));
  return result;
}
function probe(label, binary) {
  const fixture = path.join(root, 'ocm-tests', label);
  fs.mkdirSync(fixture, {recursive: true});
  const result = cp.spawnSync(process.execPath,
    [fixtureDriver, binary, fixture, '--ordinary-probe'],
    {cwd: repository, input: 'owned\n', encoding: 'utf8', timeout: 120000, maxBuffer: 4 * 1024 * 1024});
  if (result.error) throw result.error;
  process.stdout.write(result.stdout);
  process.stderr.write(result.stderr);
  assert.equal(result.status, 0, 'Probe transport/setup failed');
  const record = result.stdout.split(/\r?\n/).filter(Boolean).map(line => JSON.parse(line))
    .find(value => value.phase === 'ordinary-build');
  assert(record, 'Probe omitted its product result');
  const markers = record.stderr.split(/\r?\n/).filter(line => /^OCM_DIGEST_(ENTER|EXIT) /.test(line));
  console.log(JSON.stringify({phase: 'probe-summary', label, exit: record.exit,
    lastMarker: markers.at(-1) || null, markers}));
  return {record, markers};
}
async function frameEvidence(target, sourceTree) {
  const deps = path.join(target, 'debug/deps');
  const files = fs.readdirSync(deps).filter(name => name.startsWith('ocm') && /\.(s|asm)$/.test(name)).sort();
  let found = false;
  for (const name of files) {
    const file = path.join(deps, name);
    const input = fs.createReadStream(file);
    const lines = readline.createInterface({input, crlfDelay: Infinity});
    let capture = null;
    for await (const line of lines) {
      // Exclude generic map_err/closure symbols that merely contain this path.
      const exactFunction = /tree_digest9hash_file(?:17h[0-9a-f]+E)?(?::|\s*$)/.test(line);
      if (!capture && exactFunction &&
          (/^\s*\.seh_proc\s/.test(line) || /^\s*\S+:/.test(line))) capture = [];
      if (capture) {
        capture.push(line);
        if (line.includes('.seh_endprologue') || capture.length === 160) {
          const allocations = capture.flatMap(value => [...value.matchAll(/\.seh_stackalloc\s+(0x[0-9a-fA-F]+|\d+)/g)]
            .map(match => match[1]));
          lines.close();
          input.destroy();
          console.log(JSON.stringify({phase: 'hash-file-frame', sourceTree, assembly: name,
            assemblySha256: await sha256(file), sehStackallocBytes: allocations,
            completePrologue: line.includes('.seh_endprologue')}));
          console.log(capture.join('\n'));
          assert(line.includes('.seh_endprologue') && allocations.length, 'Incomplete native frame evidence');
          found = true;
          break;
        }
      }
    }
    if (found) break;
  }
  assert(found, 'No emitted native hash_file prologue found');
}
async function main() {
  console.log(JSON.stringify({phase: 'qualification-input', revision, root, assemblyOnly,
    driverSha256: await sha256(__filename), fixtureDriverSha256: await sha256(fixtureDriver),
    node: process.version, nodeBinarySha256: await sha256(process.execPath),
    rustflags: process.env.RUSTFLAGS || null, encodedRustflags: process.env.CARGO_ENCODED_RUSTFLAGS || null,
    rustMinStack: process.env.RUST_MIN_STACK || null}));
  run('rustc', ['-vV']);
  run('cargo', ['--version']);
  run('git', ['fetch', '--no-tags', '--depth=1', 'origin', revision]);
  const baseline = path.join(root, 'baseline');
  const diagnostic = path.join(root, 'diagnostic');
  for (const checkout of assemblyOnly ? [diagnostic] : [baseline, diagnostic]) {
    run('git', ['worktree', 'add', '--detach', checkout, revision]);
    assert.equal(git(['rev-parse', 'HEAD'], checkout), revision);
    assert.equal(git(['status', '--porcelain'], checkout), '');
  }
  if (!assemblyOnly) {
    const baselineTarget = path.join(root, 'baseline-target');
    const baselineEnv = {...process.env, CARGO_TARGET_DIR: baselineTarget};
    run('cargo', ['build', '--locked', '--bin', 'ocm', '--manifest-path', path.join(baseline, 'Cargo.toml')],
      {env: baselineEnv});
    const baselineBinary = path.join(baselineTarget, 'debug/ocm.exe');
    await identity('baseline', baseline, baselineBinary);
    probe('baseline', baselineBinary);
  }

  const sourceFile = path.join(diagnostic, 'src/infra/tree_digest.rs');
  const source = fs.readFileSync(sourceFile, 'utf8');
  const anchor = '        ("file", None, Some(hash_file(path)?))';
  assert.equal(source.split(anchor).length, 2, 'Exact invocation marker anchor changed');
  fs.writeFileSync(sourceFile, source.replace(anchor,
    '        eprintln!("OCM_DIGEST_ENTER {}", path.display());\n' +
    '        let digest = hash_file(path);\n' +
    '        eprintln!("OCM_DIGEST_EXIT {}", path.display());\n' +
    '        ("file", None, Some(digest?))'));
  run('git', ['add', 'src/infra/tree_digest.rs'], {cwd: diagnostic});
  run('git', ['diff', '--cached', '--', 'src/infra/tree_digest.rs'], {cwd: diagnostic});
  const diagnosticTarget = path.join(root, 'diagnostic-target');
  const diagnosticEnv = {...process.env, CARGO_TARGET_DIR: diagnosticTarget};
  if (assemblyOnly) {
    const tree = git(['write-tree'], diagnostic);
    assert.equal(tree, expectedDiagnosticTree, 'Diagnostic source differs from the native run');
    console.log(JSON.stringify({phase: 'assembly-source', head: revision, tree,
      status: git(['status', '--porcelain'], diagnostic),
      digestSourceSha256: await sha256(sourceFile)}));
    run('cargo', ['rustc', '--locked', '--lib', '--manifest-path', path.join(diagnostic, 'Cargo.toml'),
      '--', '--emit=asm,link'], {env: diagnosticEnv});
    await frameEvidence(diagnosticTarget, tree);
    return;
  }
  run('cargo', ['build', '--locked', '--bin', 'ocm', '--manifest-path', path.join(diagnostic, 'Cargo.toml')],
    {env: diagnosticEnv});
  const diagnosticBinary = path.join(diagnosticTarget, 'debug/ocm.exe');
  const diagnosticIdentity = await identity('diagnostic', diagnostic, diagnosticBinary);
  const result = probe('diagnostic', diagnosticBinary);
  if (/^OCM_DIGEST_ENTER /.test(result.markers.at(-1) || '')) {
    // Assembly is emitted from the same diagnostic source/default dev profile;
    // the already-executed executable is retained and checked unchanged.
    run('cargo', ['rustc', '--locked', '--lib', '--manifest-path', path.join(diagnostic, 'Cargo.toml'),
      '--', '--emit=asm,link'], {env: diagnosticEnv});
    assert.equal(await sha256(diagnosticBinary), diagnosticIdentity.binarySha256);
    await frameEvidence(diagnosticTarget, diagnosticIdentity.tree);
  } else {
    console.log(JSON.stringify({phase: 'assembly-not-requested', reason: 'ENTER was not the last marker'}));
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
