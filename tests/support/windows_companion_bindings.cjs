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

function treeSnapshot(directory, allowLinks = false) {
  const files = {};
  const rootReal = fs.realpathSync(directory);
  function visit(current) {
    for (const name of fs.readdirSync(current).sort()) {
      const file = path.join(current, name);
      const stat = fs.lstatSync(file);
      const relative = path.relative(directory, file);
      if (stat.isSymbolicLink()) {
        assert.ok(allowLinks, `Unexpected source link: ${file}`);
        const resolved = path.relative(rootReal, fs.realpathSync(file));
        assert.ok(!path.isAbsolute(resolved) && resolved !== '..'
          && !resolved.startsWith(`..${path.sep}`), `Runtime link escaped its published tree: ${file}`);
        files[relative] = {link: fs.readlinkSync(file)};
      } else if (stat.isDirectory()) {
        files[relative] = {directory: true};
        visit(file);
      } else if (stat.isFile()) {
        files[relative] = {sha256: fileDigest(file)};
      } else {
        assert.fail(`Unexpected fixture entry: ${file}`);
      }
    }
  }
  visit(directory);
  return files;
}

function assertNoRuntimeScratch(name, installed) {
  assert.ok(!fs.existsSync(path.join(installed, 'pack')), 'Published runtime retained packing scratch');
  const leftovers = fs.readdirSync(path.join(env.OCM_HOME, 'runtimes')).filter(entry =>
    ['stage', 'pack', 'backup'].some(kind => entry.startsWith(`.${name}.${kind}-`))
    || entry.startsWith(`.${name}.json.backup-`));
  assert.deepEqual(leftovers, [], 'Runtime operation left a temporary stage or backup');
}

function main() {
  console.log(JSON.stringify({platform: process.platform, release: os.release(), version: os.version(),
    architecture: process.arch, node: process.version, nodeBinary: process.execPath,
    nodeSha256: fileDigest(process.execPath), binary, binarySha256: fileDigest(binary),
    scriptSha256: fileDigest(__filename)}));
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
  const cyclicBridgeArchives = {};
  for (const name of ['bridge-a', 'bridge-b']) {
    const directory = path.join(packages, name);
    writePackage(directory, {name, version: '1.0.0', main: 'index.cjs',
      dependencies: {value: leafArchive}}, "module.exports=require('value');\n");
    bridgeArchives[name] = pack(directory);
    const cyclicName = `cyclic-${name}`;
    const cyclicDirectory = path.join(packages, cyclicName);
    writePackage(cyclicDirectory, {name: cyclicName, version: '1.0.0', main: 'index.cjs',
      dependencies: {value: leafArchive}, peerDependencies: {'@openclaw/codex': '*'}},
      "module.exports={value:require('value'),token:require('@openclaw/codex').token};\n");
    cyclicBridgeArchives[cyclicName] = pack(cyclicDirectory);
  }

  // Both the native prefixes and the generated host inspect the same exported
  // graph facts in one process, including identity between separate companions.
  const inspectCompanions = `function inspect(pluginPaths) {
  const plugins=pluginPaths.map(file=>require(file));
  console.log(JSON.stringify({companions:plugins.map(plugin=>plugin.observation),
    isolated:plugins.every((plugin,index)=>plugins.slice(0,index).every(previous=>
      plugin.token!==previous.token && plugin.direct!==previous.direct && plugin.common!==previous.common))}));
}`;
  const failures = [];
  const cases = [
    {name: 'plain-hoisted', strategy: 'hoisted', bundled: false},
    {name: 'plain-nested', strategy: 'nested', bundled: false},
    {name: 'bundled-hoisted', strategy: 'hoisted', bundled: true},
  ];
  for (const scenario of cases) {
    const startedAt = Date.now();
    const repo = path.join(root, scenario.name);
    fs.mkdirSync(repo);
    const selectedEnv = {...env, npm_config_install_strategy: scenario.strategy};
    const version = '2026.10.8';
    const companionIds = scenario.bundled ? ['codex', 'second'] : ['codex'];
    fs.writeFileSync(path.join(repo, 'package.json'), JSON.stringify({
      name: 'openclaw', version, type: 'module', engines: {node: '>=22.22.3'},
      bin: {openclaw: 'openclaw.mjs'}, files: ['openclaw.mjs', 'sdk.cjs'],
      exports: {'./sdk': './sdk.cjs'},
    }));
    fs.writeFileSync(path.join(repo, 'openclaw.mjs'),
      "#!/usr/bin/env node\nimport {createRequire} from 'node:module'; const require=createRequire(import.meta.url);\n"
      + inspectCompanions + `\ninspect(${JSON.stringify(companionIds.map(id => `./dist/extensions/${id}`))});\n`);
    fs.writeFileSync(path.join(repo, 'sdk.cjs'), "module.exports='host-sdk-1';\n");
    // npm resolves peer metadata even with --omit=peer. Supply the declared
    // host archive for the offline native control, as in the Unix fixture.
    const hostArchive = scenario.bundled ? pack(repo) : undefined;
    const companion = path.join(repo, 'extensions/codex');
    const manifest = {
      name: '@openclaw/codex', version, main: 'index.cjs',
      files: ['index.cjs', 'openclaw.plugin.json'],
      dependencies: {value: scenario.bundled ? '1.0.0' : leafArchive,
        ...(scenario.bundled ? cyclicBridgeArchives : bridgeArchives)},
      openclaw: {build: {openclawVersion: version}, release: {publishToNpm: true},
        runtimeExtensions: ['./index.cjs']},
    };
    if (scenario.bundled) {
      manifest.bundleDependencies = ['value'];
      manifest.peerDependencies = {openclaw: hostArchive};
    }
    const companionSource = scenario.bundled
      ? "exports.token={}; const direct=require('value'),a=require('cyclic-bridge-a'),b=require('cyclic-bridge-b'); exports.direct=direct; exports.common=a.value; exports.observation={id:'codex',generation:1,values:[direct.version,a.value.version,b.value.version],shared:a.value===b.value,private:direct!==a.value,cycle:a.token===exports.token && b.token===exports.token,peer:require('openclaw/sdk')};\n"
      : "exports.token={}; const direct=require('value'),a=require('bridge-a'),b=require('bridge-b'); exports.direct=direct; exports.common=direct; exports.observation={id:'codex',generation:1,values:[direct.version,a.version,b.version],shared:a===b,private:direct!==a};\n";
    writePackage(companion, manifest, companionSource);
    if (scenario.bundled) writePackage(path.join(companion, 'node_modules/value'), leafManifest, leafSource);
    fs.writeFileSync(path.join(companion, 'openclaw.plugin.json'), JSON.stringify({id: 'codex', configSchema: {}}));
    const second = path.join(repo, 'extensions/second');
    const secondManifest = {
      ...manifest, name: '@openclaw/second', dependencies: {value: leafArchive},
    };
    delete secondManifest.bundleDependencies;
    const secondSource = "exports.token={}; exports.direct=require('value'); exports.common=exports.direct; exports.observation={id:'second',generation:1,value:exports.direct.version,peer:require('openclaw/sdk')};\n";
    if (scenario.bundled) {
      writePackage(second, secondManifest, secondSource);
      fs.writeFileSync(path.join(second, 'openclaw.plugin.json'), JSON.stringify({id: 'second', configSchema: {}}));
    }
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
    const nativePaths = [];
    for (const id of companionIds) {
      const archive = pack(path.join(repo, 'extensions', id));
      const nativePrefix = path.join(root, `native-${scenario.name}-${id}`);
      npm(['install', '--prefix', nativePrefix, '--omit=dev', '--omit=peer', '--no-save',
        '--package-lock=false', archive, ...(hostArchive ? [hostArchive] : [])], {env: selectedEnv});
      nativePaths.push(path.join(nativePrefix, 'node_modules/@openclaw', id));
    }
    let expectedSource = treeSnapshot(repo);
    console.log(JSON.stringify({scenario, fixtureSha256: digest(JSON.stringify(expectedSource)), sourceFiles: expectedSource}));
    const native = JSON.parse(run(process.execPath,
      ['-e', inspectCompanions + '\ninspect(process.argv.slice(1));', ...nativePaths],
      {env: selectedEnv}));
    assert.deepEqual(native.companions[0].values, [1, 1, 1]);
    if (scenario.bundled) {
      assert.equal(native.companions[0].shared, true, 'Native bridges must share the hoisted module');
      assert.equal(native.companions[0].private, true, 'Native bundled and hoisted modules must remain distinct');
      assert.equal(native.companions[0].cycle, true, 'Native dependency cycles must return the owning companion');
      assert.equal(native.isolated, true, 'Independent native companion prefixes must retain separate identity');
      for (const plugin of native.companions) assert.equal(plugin.peer, 'host-sdk-1');
    }
    try {
      const buildArgs = ['runtime', 'build-local', scenario.name, '--repo', repo,
        ...companionIds.flatMap(id => ['--companion', id]), '--json'];
      const metadata = JSON.parse(run(binary, buildArgs, {env: selectedEnv}));
      const installed = path.resolve(metadata.installRoot);
      assert.equal(fs.realpathSync(installed).toLowerCase(),
        fs.realpathSync(path.join(env.OCM_HOME, 'runtimes', scenario.name)).toLowerCase());
      assertNoRuntimeScratch(scenario.name, installed);
      const metadataPath = path.join(env.OCM_HOME, 'runtimes', `${scenario.name}.json`);
      let bound = false;
      function observeRuntime(currentMetadata, expected, phase) {
        assertNoRuntimeScratch(scenario.name, installed);
        assert.deepEqual(currentMetadata.companions.map(plugin => plugin.id), companionIds);
        const metadataSha256 = fileDigest(metadataPath);
        const verification = observe(binary, ['runtime', 'verify', scenario.name, '--json'], {env: selectedEnv});
        assert.ok([0, 1].includes(verification.status), 'Unexpected verification process outcome');
        const verified = JSON.parse(verification.stdout);
        assert.equal(verified.healthy, verification.status === 0);
        // The existing Windows entrypoint spelling defect is independent of binding.
        // Retain its exact failure; do not accept other integrity failures here.
        if (!verified.healthy) {
          assert.equal(verified.issue,
            `runtime companion codex has invalid entrypoint path: ${currentMetadata.companions[0].entrypoint}`);
        }
        console.log(JSON.stringify({scenario: scenario.name, phase, verificationExit: verification.status,
          verificationHealthy: verified.healthy, verificationIssue: verified.issue ?? null}));
        const materialized = JSON.parse(bound
          ? run(binary, [`@${scenario.name}`, '--', '--version'], {env: selectedEnv})
          : run(process.execPath, [currentMetadata.binaryPath, '--version'], {env: selectedEnv}));
        console.log(JSON.stringify({scenario, phase, native, expected, materialized}));
        assert.deepEqual(materialized, expected, 'Materialization changed the native dependency graph');
        assert.equal(fileDigest(metadataPath), metadataSha256, 'Product observations rewrote generated runtime metadata');
        assert.deepEqual(treeSnapshot(repo), expectedSource, 'OCM changed the source fixture');
        // Record links without following directory cycles; every resolved target
        // must now belong to the published runtime, never its removed stage.
        return treeSnapshot(installed, true);
      }
      if (!scenario.bundled) {
        run(binary, ['env', 'create', scenario.name, '--runtime', scenario.name], {env: selectedEnv});
        bound = true;
      }
      const initialTree = observeRuntime(metadata, native, 'initial');
      if (scenario.bundled) {
        assert.ok(Object.values(initialTree).some(entry => entry.link),
          'The conflicting graph must retain its shared packages through directory bindings');
        // Keep the runtime unbound: --force correctly refuses a bound runtime.
        // New code and host SDK markers distinguish replacement from retaining
        // the old tree, without changing the dependency graph under comparison.
        fs.writeFileSync(path.join(companion, 'index.cjs'), companionSource + 'exports.observation.generation=2;\n');
        fs.writeFileSync(path.join(second, 'index.cjs'), secondSource + 'exports.observation.generation=2;\n');
        fs.writeFileSync(path.join(repo, 'sdk.cjs'), "module.exports='host-sdk-2';\n");
        expectedSource = treeSnapshot(repo);
        const replacement = JSON.parse(run(binary, [...buildArgs, '--force'], {env: selectedEnv}));
        assert.equal(path.resolve(replacement.installRoot).toLowerCase(), installed.toLowerCase());
        assert.notEqual(replacement.runtimeSha256, metadata.runtimeSha256, 'Replacement retained the old runtime digest');
        const expectedReplacement = {...native,
          companions: native.companions.map(plugin => ({...plugin, generation: 2, peer: 'host-sdk-2'}))};
        const replacementTree = observeRuntime(replacement, expectedReplacement, 'replacement');
        const replacementMetadataSha256 = fileDigest(metadataPath);

        // Fail in the second companion after both dependency graphs have been
        // materialized. The previously published runtime must remain usable.
        const invalidManifest = {...secondManifest,
          openclaw: {...secondManifest.openclaw, runtimeExtensions: ['./missing.cjs']}};
        fs.writeFileSync(path.join(second, 'package.json'), JSON.stringify(invalidManifest));
        expectedSource = treeSnapshot(repo);
        const failed = observe(binary, [...buildArgs, '--force'], {env: selectedEnv});
        assert.equal(failed.status, 1, 'The missing companion entry must reject replacement');
        assert.match(failed.stderr, /missing runtime entrypoint/);
        assertNoRuntimeScratch(scenario.name, installed);
        assert.equal(fileDigest(metadataPath), replacementMetadataSha256, 'Failed build changed the published metadata');
        assert.deepEqual(treeSnapshot(installed, true), replacementTree,
          'Failed build changed the published files or directory bindings');
        run(binary, ['env', 'create', scenario.name, '--runtime', scenario.name], {env: selectedEnv});
        bound = true;
        const preservedTree = observeRuntime(replacement, expectedReplacement, 'failed-replacement-preserved');
        assert.deepEqual(preservedTree, replacementTree, 'Observing the preserved runtime changed its tree');
      }
    } catch (error) {
      console.error(error);
      failures.push({scenario: scenario.name, failure: String(error)});
    } finally {
      assert.deepEqual(treeSnapshot(repo), expectedSource, 'OCM changed the source fixture');
      console.log(JSON.stringify({scenario: scenario.name, sourceBytesUnchanged: true,
        elapsedMs: Date.now() - startedAt}));
    }
  }
  console.log(JSON.stringify({nativeWindowsCompanionObservationsComplete: true, failures}));
  assert.deepEqual(failures, [], 'Native companion dependency checks failed');
  console.log(JSON.stringify({nativeWindowsCompanionWithoutSymlinkPrivilege: 'passed'}));
}

try {
  main();
} catch (error) {
  console.error(error);
  process.exitCode = 1;
}
