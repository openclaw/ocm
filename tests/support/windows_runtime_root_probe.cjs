'use strict';

const assert = require('node:assert/strict');
const cp = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

assert.equal(process.platform, 'win32');
const [binary, root] = process.argv.slice(2).map(value => path.resolve(value));
assert.equal(path.basename(path.dirname(root)), 'ocm-tests');
const hash = file => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const env = {};
for (const key of ['PATH', 'SystemRoot', 'WINDIR', 'COMSPEC', 'PATHEXT']) {
  const entry = Object.entries(process.env).find(([name]) => name.toUpperCase() === key.toUpperCase());
  if (entry) env[key] = entry[1];
}
Object.assign(env, {
  HOME: path.join(root, 'home'), USERPROFILE: path.join(root, 'home'),
  APPDATA: path.join(root, 'home/AppData/Roaming'),
  LOCALAPPDATA: path.join(root, 'home/AppData/Local'),
  TEMP: path.join(root, 'tmp'), TMP: path.join(root, 'tmp'),
  OCM_HOME: path.join(root, 'store'), OCM_INTERNAL_SKIP_SERVICE_READINESS: '1',
  npm_config_cache: path.join(root, 'cache'), npm_config_offline: 'true',
  npm_config_audit: 'false', npm_config_fund: 'false', npm_config_update_notifier: 'false',
});
for (const key of ['HOME', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'OCM_HOME', 'npm_config_cache']) fs.mkdirSync(env[key], {recursive: true});
function observe(command, args, options = {}) {
  const result = cp.spawnSync(command, args, {cwd: options.cwd || root, env,
    encoding: 'utf8', timeout: 60000, maxBuffer: 4 * 1024 * 1024});
  console.log(JSON.stringify({command, args, exit: result.status,
    error: result.error ? String(result.error) : null, stdout: result.stdout || '', stderr: result.stderr || ''}));
  if (result.error) throw result.error;
  return result;
}
function run(command, args, options) {
  const result = observe(command, args, options);
  assert.equal(result.status, 0, result.stderr);
  return result.stdout;
}
console.log(JSON.stringify({probe: 'host-only-publication', node: process.version,
  binary, binarySha256: hash(binary), scriptSha256: hash(__filename)}));
const npmCommand = run('where.exe', ['npm.cmd']).trim().split(/\r?\n/)[0];
const npmCli = path.join(path.dirname(npmCommand), 'node_modules/npm/bin/npm-cli.js');
env.OCM_INTERNAL_NPM_BIN = npmCommand;
console.log(JSON.stringify({npmCommand, npmCli, npmCliSha256: hash(npmCli)}));
run(process.execPath, [npmCli, '--version']);
const archives = path.join(root, 'archives');
fs.mkdirSync(archives);
const failures = [];
for (const strategy of ['hoisted', 'linked']) {
  const name = `host-${strategy}`;
  const repo = path.join(root, name);
  fs.mkdirSync(repo);
  fs.writeFileSync(path.join(repo, 'package.json'), JSON.stringify({name: 'openclaw',
    version: '2026.10.8', type: 'module', engines: {node: '>=22.22.3'},
    bin: {openclaw: 'openclaw.mjs'}, files: ['openclaw.mjs']}));
  fs.writeFileSync(path.join(repo, 'openclaw.mjs'), "console.log('host-only');\n");
  const before = ['package.json', 'openclaw.mjs'].map(file => hash(path.join(repo, file)));
  env.npm_config_install_strategy = strategy;
  let phase = 'native-prerequisite';
  try {
    const packed = JSON.parse(run(process.execPath, [npmCli, 'pack', '--json', '--ignore-scripts', '--pack-destination', archives], {cwd: repo}));
    const requestedPrefix = path.join(root, `native-${strategy}`);
    fs.mkdirSync(requestedPrefix);
    const prefix = fs.realpathSync.native(requestedPrefix);
    run(process.execPath, [npmCli, 'install', '--prefix', prefix, '--omit=dev', '--no-save', '--package-lock=false', `file:${path.join(archives, packed[0].filename)}`]);
    assert.equal(run(process.execPath, [path.join(prefix, 'node_modules/openclaw/openclaw.mjs')]).trim(), 'host-only');
    phase = 'ocm-build';
    const metadata = JSON.parse(run(binary, ['runtime', 'build-local', name, '--repo', repo, '--json']));
    const metadataPath = path.join(env.OCM_HOME, 'runtimes', `${name}.json`);
    const metadataHash = hash(metadataPath);
    const packageRoot = path.join(metadata.installRoot, 'files/node_modules/openclaw');
    const stat = fs.lstatSync(packageRoot);
    const linkTarget = stat.isSymbolicLink() ? fs.readlinkSync(packageRoot) : null;
    phase = 'ocm-publication';
    const verification = observe(binary, ['runtime', 'verify', name, '--json']);
    const verified = JSON.parse(verification.stdout);
    const entry = observe(process.execPath, [metadata.binaryPath]);
    const binding = observe(binary, ['env', 'create', name, '--runtime', name]);
    const alias = binding.status === 0 ? observe(binary, [`@${name}`, '--', '--version']) : null;
    console.log(JSON.stringify({probe: 'host-only-publication', strategy, metadata,
      linkTarget, binaryExists: fs.existsSync(metadata.binaryPath),
      stageInTarget: linkTarget?.includes(`.${name}.stage-`) ?? false,
      verifyExit: verification.status, verified, entryExit: entry.status,
      bindingExit: binding.status, aliasExit: alias?.status ?? null,
      metadataUnchanged: hash(metadataPath) === metadataHash}));
    assert.equal(hash(metadataPath), metadataHash);
    assert.equal(verification.status, 0);
    assert.equal(verified.healthy, true);
    assert.equal(entry.status, 0);
    assert.equal(entry.stdout.trim(), 'host-only');
    assert.equal(binding.status, 0);
    assert.equal(alias.status, 0);
    assert.equal(linkTarget?.includes(`.${name}.stage-`) ?? false, false);
  } catch (error) {
    const failure = {strategy, phase, error: String(error)};
    console.log(JSON.stringify(failure));
    failures.push(failure);
  } finally {
    assert.deepEqual(['package.json', 'openclaw.mjs'].map(file => hash(path.join(repo, file))), before);
    console.log(JSON.stringify({strategy, sourceBytesUnchanged: true}));
  }
}
console.log(JSON.stringify({nativeWindowsRootPublicationObservationsComplete: true, failures}));
assert.deepEqual(failures, []);
