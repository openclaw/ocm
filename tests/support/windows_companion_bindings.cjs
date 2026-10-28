'use strict';

const assert = require('node:assert/strict');
const cp = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

assert.equal(process.platform, 'win32', 'This proof requires native Windows');
assert.ok(process.argv[2] && process.argv[3], 'Pass the exact OCM binary and TestDir');
const binary = path.resolve(process.argv[2]);
const root = path.resolve(process.argv[3]);
assert.equal(path.basename(path.dirname(root)), 'ocm-tests');
assert.ok(fs.statSync(binary).isFile());
assert.ok(fs.statSync(root).isDirectory());
const baselineValues = process.env.OCM_TEST_COMPANION_BASELINE_VALUES === '1';
const captureOnly = process.env.OCM_TEST_COMPANION_CAPTURE_ONLY === '1';
const digest = value => crypto.createHash('sha256').update(value).digest('hex');
const fileDigest = file => digest(fs.readFileSync(file));

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
  OCM_HOME: path.join(root, 'ocm-home'), OCM_INTERNAL_SKIP_SERVICE_READINESS: '1',
  npm_config_cache: path.join(root, 'cache'), npm_config_offline: 'true',
  npm_config_audit: 'false', npm_config_fund: 'false',
  npm_config_update_notifier: 'false',
});
for (const key of ['HOME', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'OCM_HOME', 'npm_config_cache']) {
  fs.mkdirSync(env[key], {recursive: true});
}

function observe(command, args, options = {}) {
  const cwd = options.cwd || root;
  console.log(JSON.stringify({command, args, cwd, windowsHide: options.windowsHide ?? false}));
  const result = cp.spawnSync(command, args, {
    cwd, env: options.env || env, encoding: 'utf8', timeout: 60000,
    maxBuffer: 4 * 1024 * 1024, windowsHide: options.windowsHide ?? false,
  });
  console.log(JSON.stringify({command, args, exit: result.status, signal: result.signal,
    error: result.error ? String(result.error) : null,
    stdout: result.stdout || '', stderr: result.stderr || ''}));
  if (result.error) throw result.error;
  return result;
}

function run(command, args, options = {}) {
  const result = observe(command, args, options);
  assert.equal(result.status, 0, `${command} failed: ${result.stderr}`);
  return result.stdout;
}

function writePackage(directory, manifest, entry) {
  fs.mkdirSync(directory, {recursive: true});
  fs.writeFileSync(path.join(directory, 'package.json'), JSON.stringify(manifest));
  fs.writeFileSync(path.join(directory, 'index.cjs'), entry);
}

function sourceSnapshot(directory) {
  const files = {};
  function visit(current) {
    for (const name of fs.readdirSync(current).sort()) {
      const file = path.join(current, name);
      const stat = fs.lstatSync(file);
      assert.ok(!stat.isSymbolicLink(), `Unexpected source link: ${file}`);
      if (stat.isDirectory()) visit(file);
      else if (stat.isFile()) files[path.relative(directory, file)] = fileDigest(file);
      else assert.fail(`Unexpected source entry: ${file}`);
    }
  }
  visit(directory);
  return files;
}

function main() {
  console.log(JSON.stringify({platform: process.platform, release: os.release(), version: os.version(),
    architecture: process.arch, node: process.version, nodeBinary: process.execPath,
    nodeSha256: fileDigest(process.execPath), binary, binarySha256: fileDigest(binary),
    scriptSha256: fileDigest(__filename), baselineValues, captureOnly}));
  const npmCommand = run('where.exe', ['npm.cmd']).trim().split(/\r?\n/)[0];
  assert.ok(path.isAbsolute(npmCommand) && fs.statSync(npmCommand).isFile());
  const npmCli = path.join(path.dirname(npmCommand), 'node_modules/npm/bin/npm-cli.js');
  assert.ok(fs.statSync(npmCli).isFile(), 'The selected native npm must provide its CLI');
  env.OCM_INTERNAL_NPM_BIN = npmCommand;
  console.log(JSON.stringify({npmCommand, npmCommandSha256: fileDigest(npmCommand),
    npmCli, npmCliSha256: fileDigest(npmCli)}));
  run(process.execPath, [npmCli, '--version']);
  run(binary, ['--version']);

  const archives = path.join(root, 'archives');
  const packages = path.join(root, 'packages');
  fs.mkdirSync(archives);
  fs.mkdirSync(packages);
  function npm(args, options = {}) {
    return run(process.execPath, [npmCli, ...args], options);
  }
  function pack(directory) {
    const packed = JSON.parse(npm(['pack', '--json', '--ignore-scripts',
      '--pack-destination', archives], {cwd: directory}));
    assert.equal(packed.length, 1);
    const archive = path.join(archives, packed[0].filename);
    console.log(JSON.stringify({archive, sha256: fileDigest(archive)}));
    return `file:${archive}`;
  }

  const leaf = path.join(packages, 'value');
  const leafManifest = {name: 'value', version: '1.0.0', main: 'index.cjs'};
  const leafSource = 'module.exports={version:1};\n';
  writePackage(leaf, leafManifest, leafSource);
  const leafArchive = pack(leaf);
  const bridgeArchives = {};
  for (const name of ['bridge-a', 'bridge-b']) {
    const directory = path.join(packages, name);
    writePackage(directory, {name, version: '1.0.0', main: 'index.cjs',
      dependencies: {value: leafArchive}}, "module.exports=require('value');\n");
    bridgeArchives[name] = pack(directory);
  }

  const cases = [
    {name: 'plain-hoisted', strategy: 'hoisted', bundled: false},
    {name: 'plain-nested', strategy: 'nested', bundled: false},
    {name: 'bundled-hoisted', strategy: 'hoisted', bundled: true},
  ];
  for (const scenario of cases.filter(value => !captureOnly || value.name === 'plain-hoisted')) {
    const repo = path.join(root, scenario.name);
    fs.mkdirSync(repo);
    const selectedEnv = {...env, npm_config_install_strategy: scenario.strategy};
    const version = '2026.10.8';
    fs.writeFileSync(path.join(repo, 'package.json'), JSON.stringify({
      name: 'openclaw', version, type: 'module', engines: {node: '>=22.22.3'},
      bin: {openclaw: 'openclaw.mjs'}, files: ['openclaw.mjs'],
    }));
    fs.writeFileSync(path.join(repo, 'openclaw.mjs'),
      "#!/usr/bin/env node\nimport {createRequire} from 'node:module'; const require=createRequire(import.meta.url); console.log(JSON.stringify(require('./dist/extensions/codex')));\n");
    const companion = path.join(repo, 'extensions/codex');
    const manifest = {
      name: '@openclaw/codex', version, main: 'index.cjs',
      files: ['index.cjs', 'openclaw.plugin.json'],
      dependencies: {value: scenario.bundled ? '1.0.0' : leafArchive, ...bridgeArchives},
      openclaw: {build: {openclawVersion: version}, release: {publishToNpm: true},
        runtimeExtensions: ['./index.cjs']},
    };
    if (scenario.bundled) manifest.bundleDependencies = ['value'];
    writePackage(companion, manifest,
      "const direct=require('value'),a=require('bridge-a'),b=require('bridge-b'); module.exports={values:[direct.version,a.version,b.version],shared:a===b,private:direct!==a};\n");
    if (scenario.bundled) writePackage(path.join(companion, 'node_modules/value'), leafManifest, leafSource);
    fs.writeFileSync(path.join(companion, 'openclaw.plugin.json'), JSON.stringify({id: 'codex', configSchema: {}}));
    fs.mkdirSync(path.join(repo, 'scripts/lib'), {recursive: true});
    for (const filename of ['scripts/lib/plugin-npm-runtime-build.mjs', 'scripts/generate-npm-package-lock.mjs']) {
      fs.writeFileSync(path.join(repo, filename), '// The isolated fixture payload is already built.\n');
    }
    // Dispatch the selected Windows npm through its real JS CLI, preserving the
    // argument array without cmd.exe expansion of fixture paths.
    fs.writeFileSync(path.join(repo, 'scripts/lib/plugin-npm-package-manifest.mjs'), `
import {spawnSync} from 'node:child_process';
import path from 'node:path';
const [,,run,directory,separator,command,...args]=process.argv;
if(run!=='--run'||separator!=='--'||path.resolve(command).toLowerCase()!==path.resolve(process.env.OCM_INTERNAL_NPM_BIN).toLowerCase()) throw Error('Unexpected native npm packaging contract');
const cli=path.join(path.dirname(command),'node_modules/npm/bin/npm-cli.js');
const result=spawnSync(process.execPath,[cli,...args],{cwd:directory,stdio:'inherit'});
if(result.error) throw result.error;
process.exit(result.status??1);
`);
    const archive = pack(companion);
    const before = sourceSnapshot(repo);
    console.log(JSON.stringify({scenario, fixtureSha256: digest(JSON.stringify(before)), sourceFiles: before}));
    const nativePrefix = path.join(root, `native-${scenario.name}`);
    npm(['install', '--prefix', nativePrefix, '--omit=dev', '--omit=peer', '--no-save',
      '--package-lock=false', archive], {env: selectedEnv});
    const native = JSON.parse(run(process.execPath,
      ['-e', 'console.log(JSON.stringify(require(process.argv[1])))', path.join(nativePrefix, 'node_modules/@openclaw/codex')],
      {env: selectedEnv}));
    assert.deepEqual(native.values, [1, 1, 1]);
    if (scenario.bundled) {
      assert.equal(native.shared, true, 'Native bridges must share the hoisted module');
      assert.equal(native.private, true, 'Native bundled and hoisted modules must remain distinct');
    }
    try {
      const metadata = JSON.parse(run(binary, ['runtime', 'build-local', scenario.name,
        '--repo', repo, '--companion', 'codex', '--json'], {env: selectedEnv}));
      const installed = path.resolve(metadata.installRoot);
      assert.equal(fs.realpathSync(installed).toLowerCase(),
        fs.realpathSync(path.join(env.OCM_HOME, 'runtimes', scenario.name)).toLowerCase());
      assert.ok(!fs.existsSync(path.join(installed, 'pack')), 'Published runtime retained packing scratch');
      const stages = fs.readdirSync(path.join(env.OCM_HOME, 'runtimes')).filter(name =>
        name.startsWith(`.${scenario.name}.stage-`) || name.startsWith(`.${scenario.name}.pack-`));
      assert.deepEqual(stages, [], 'Successful publication left a temporary stage');
      if (captureOnly) {
        const metadataPath = path.join(env.OCM_HOME, 'runtimes', `${scenario.name}.json`);
        const metadataSha256 = fileDigest(metadataPath);
        const observed = observe(binary, ['runtime', 'verify', scenario.name, '--json'], {env: selectedEnv});
        assert.ok([0, 1].includes(observed.status), 'Unexpected verification process outcome');
        const verification = JSON.parse(observed.stdout);
        assert.equal(typeof verification.healthy, 'boolean');
        assert.equal(fileDigest(metadataPath), metadataSha256, 'Verification rewrote generated metadata');
        console.log(JSON.stringify({captureOnly: true, scenario: scenario.name,
          buildExit: 0, verifyExit: observed.status,
          emittedEntrypoint: metadata.companions[0].entrypoint,
          metadataSha256, generatedMetadataUnchanged: true, verification}));
        // These are independent product commands, not a successful verification.
        // Keep the captured diagnostic intact and observe whether binding can run.
        const binding = observe(binary, ['env', 'create', scenario.name, '--runtime', scenario.name], {env: selectedEnv});
        const entrypoint = binding.status === 0 ? observe(binary,
          [`@${scenario.name}`, '--', '--version'], {env: selectedEnv}) : null;
        console.log(JSON.stringify({independentBindingObservation: true,
          verifyExit: observed.status, verificationHealthy: verification.healthy,
          bindingExit: binding.status, entrypointExit: entrypoint?.status ?? null,
          entrypointStdout: entrypoint?.stdout ?? null,
          entrypointStderr: entrypoint?.stderr ?? null}));
        assert.equal(fileDigest(metadataPath), metadataSha256, 'Binding observation rewrote generated runtime metadata');
        continue;
      }
      const verified = JSON.parse(run(binary, ['runtime', 'verify', scenario.name, '--json'], {env: selectedEnv}));
      assert.equal(verified.healthy, true);
      run(binary, ['env', 'create', scenario.name, '--runtime', scenario.name], {env: selectedEnv});
      const materialized = JSON.parse(run(binary, [`@${scenario.name}`, '--', '--version'], {env: selectedEnv}));
      console.log(JSON.stringify({scenario, native, materialized, baselineValues}));
      if (baselineValues) {
        // Temporary predecessor comparison: expose the complete result while
        // checking the working values and shared bridges, without hiding the
        // predecessor's separate bundled-versus-hoisted identity defect.
        assert.deepEqual(materialized.values, native.values);
        assert.equal(materialized.shared, native.shared);
      } else {
        assert.deepEqual(materialized, native, 'Materialization changed the native dependency graph');
      }
    } finally {
      assert.deepEqual(sourceSnapshot(repo), before, 'OCM changed the source fixture');
      console.log(JSON.stringify({scenario: scenario.name, sourceBytesUnchanged: true}));
    }
  }
  console.log(JSON.stringify(captureOnly ? {nativeWindowsCompanionCapture: 'captured', captureOnly: true} :
    {nativeWindowsCompanionWithoutSymlinkPrivilege: 'passed', baselineValues}));
}

try {
  main();
} catch (error) {
  console.error(error);
  process.exitCode = 1;
}
