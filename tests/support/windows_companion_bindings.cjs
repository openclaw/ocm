'use strict';

const assert = require('node:assert/strict');
const cp = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {pathToFileURL} = require('node:url');

assert.equal(process.platform, 'win32', 'This proof requires native Windows');
assert.ok(process.argv[2] && process.argv[3], 'Pass the exact OCM binary and TestDir');
const binary = path.resolve(process.argv[2]);
const root = path.resolve(process.argv[3]);
assert.equal(path.basename(path.dirname(root)), 'ocm-tests');
assert.ok(fs.statSync(binary).isFile());
assert.ok(fs.statSync(root).isDirectory());
const digest = value => crypto.createHash('sha256').update(value).digest('hex');
const fileDigest = file => digest(fs.readFileSync(file));
const openclawSource = process.env.OCM_TEST_OPENCLAW_SOURCE;
const readerScript = path.join(__dirname, 'windows_companion_reader.mjs');

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
  const rootReal = fs.realpathSync.native(directory);
  function visit(current) {
    for (const name of fs.readdirSync(current).sort()) {
      const file = path.join(current, name);
      const stat = fs.lstatSync(file);
      const relative = path.relative(directory, file);
      if (stat.isSymbolicLink()) {
        assert.ok(allowLinks, `Unexpected source link: ${file}`);
        const resolved = path.relative(rootReal, fs.realpathSync.native(file));
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
    scriptSha256: fileDigest(__filename),
    actualReader: openclawSource ? {sourceRoot: openclawSource, scriptSha256: fileDigest(readerScript)} : null}));
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

  // Raw imports compare npm's graph, including identity between companions.
  // They are a control, not OpenClaw's final admission of a plugin entry.
  const inspectCompanions = `function inspect(pluginPaths) {
  let graph;
  try {
    // Mirror the native canonical entry returned by OpenClaw's final admission.
    // The production-loader preload separately qualifies this fixture boundary.
    const fs=require('node:fs');
    const plugins=pluginPaths.map(file=>require(fs.realpathSync.native(require.resolve(file))));
    graph={companions:plugins.map(plugin=>plugin.observation),
      isolated:plugins.every((plugin,index)=>plugins.slice(0,index).every(previous=>
        plugin.token!==previous.token && plugin.direct!==previous.direct && plugin.common!==previous.common))};
  } finally {
    // Observe only after loading and computing graph facts. Diagnostics neither
    // canonicalize import paths nor load additional fixture modules.
    try {
      const fs=require('node:fs'), path=require('node:path');
      const identities=new Map();
      const identity=value=>{
        if(!value || !['object','function'].includes(typeof value)) return null;
        if(!identities.has(value)) identities.set(value,identities.size+1);
        return identities.get(value);
      };
      const attempt=read=>{
        try {return {value:read()};}
        catch(error) {return {code:error.code??null,message:String(error.message).slice(0,300)};}
      };
      const spelling=filename=>({filename,
        realpath:attempt(()=>fs.realpathSync(filename)),
        nativeRealpath:attempt(()=>fs.realpathSync.native(filename))});
      const fixturePackages=['value','bridge-a','bridge-b','cyclic-bridge-a','cyclic-bridge-b','codex','second'];
      const entries=Object.entries(require.cache).filter(([,entry])=>
        (path.basename(entry.filename)==='index.cjs'
          && fixturePackages.includes(path.basename(path.dirname(entry.filename))))
        || (path.basename(entry.filename)==='sdk.cjs' && path.basename(path.dirname(entry.filename))==='openclaw'));
      const modules=entries.slice(0,32).map(([cacheKey,entry])=>{
        return {cacheKey,nativeRealpath:attempt(()=>fs.realpathSync.native(entry.filename)),
          exportsId:identity(entry.exports),tokenId:identity(entry.exports?.token),
          directId:identity(entry.exports?.direct),commonId:identity(entry.exports?.common),
          valueId:identity(entry.exports?.value),pluginId:entry.exports?.observation?.id??null};
      });
      console.error(JSON.stringify({moduleIdentityDiagnostics:true,
        requested:pluginPaths.map(requested=>({requested,
          resolved:attempt(()=>spelling(require.resolve(requested)))})),
        modules,omittedModules:Math.max(0,entries.length-32)}));
    } catch(error) {
      console.error(JSON.stringify({moduleIdentityDiagnostics:true,error:String(error)}));
    }
  }
  console.log(JSON.stringify(graph));
}`;
  // Capture only exports admitted and registered by the real loader. Requiring
  // the logical path again in the reader could create the alias under test.
  const registrationSource = `
exports.id=exports.observation.id;
exports.register=()=>{
  const registrations=globalThis[Symbol.for('ocm.r4.registrations')];
  if(!Array.isArray(registrations)) throw new Error('Missing fixture reader registration capture');
  registrations.push({id:exports.id,observation:exports.observation,
    token:exports.token,direct:exports.direct,common:exports.common,filename:__filename});
};
`;
  const failures = [];
  const cases = [
    {name: 'plain-hoisted', strategy: 'hoisted', bundled: false},
    {name: 'plain-nested', strategy: 'nested', bundled: false},
    {name: 'bundled-hoisted', strategy: 'hoisted', bundled: true, lifecycle: true},
    {name: 'plain-linked', strategy: 'linked', bundled: false},
    {name: 'bundled-linked', strategy: 'linked', bundled: true},
  ];
  // Temporary comparison selection; remove after the bounded baseline runs.
  const requestedCases = process.env.OCM_TEST_COMPANION_CASES;
  const selectedNames = requestedCases === undefined
    ? cases.map(scenario => scenario.name) : requestedCases.split(',').map(name => name.trim());
  assert.ok(selectedNames.length > 0 && selectedNames.every(name => cases.some(scenario => scenario.name === name)),
    'OCM_TEST_COMPANION_CASES must name existing fixture cases');
  assert.equal(new Set(selectedNames).size, selectedNames.length, 'Fixture case selection contains duplicates');
  const selectedCases = cases.filter(scenario => selectedNames.includes(scenario.name));
  console.log(JSON.stringify({selectedCases: selectedNames, allCases: selectedCases.length === cases.length}));
  for (const scenario of selectedCases) {
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
    let hostArchive;
    try {
      hostArchive = scenario.bundled ? pack(repo) : undefined;
    } catch (error) {
      console.error(error);
      const failure = {scenario: scenario.name, phase: 'native-prerequisite',
        step: 'host-pack', failure: String(error)};
      failures.push(failure);
      console.log(JSON.stringify({...failure, elapsedMs: Date.now() - startedAt}));
      continue;
    }
    const companion = path.join(repo, 'extensions/codex');
    const manifest = {
      name: '@openclaw/codex', version, main: 'index.cjs',
      files: ['index.cjs', 'openclaw.plugin.json'],
      dependencies: {value: scenario.bundled ? '1.0.0' : leafArchive,
        ...(scenario.bundled ? cyclicBridgeArchives : bridgeArchives)},
      openclaw: {build: {openclawVersion: version}, release: {publishToNpm: true},
        extensions: ['./index.cjs'], runtimeExtensions: ['./index.cjs']},
    };
    if (scenario.bundled) {
      manifest.bundleDependencies = ['value'];
      manifest.peerDependencies = {openclaw: hostArchive};
    }
    const companionSource = (scenario.bundled
      ? "exports.token={}; const direct=require('value'),a=require('cyclic-bridge-a'),b=require('cyclic-bridge-b'); exports.direct=direct; exports.common=a.value; exports.observation={id:'codex',generation:1,values:[direct.version,a.value.version,b.value.version],shared:a.value===b.value,private:direct!==a.value,cycle:a.token===exports.token && b.token===exports.token,peer:require('openclaw/sdk')};\n"
      : "exports.token={}; const direct=require('value'),a=require('bridge-a'),b=require('bridge-b'); exports.direct=direct; exports.common=direct; exports.observation={id:'codex',generation:1,values:[direct.version,a.version,b.version],shared:a===b,private:direct!==a};\n") + registrationSource;
    writePackage(companion, manifest, companionSource);
    if (scenario.bundled) writePackage(path.join(companion, 'node_modules/value'), leafManifest, leafSource);
    fs.writeFileSync(path.join(companion, 'openclaw.plugin.json'), JSON.stringify({id: 'codex', configSchema: {}}));
    const second = path.join(repo, 'extensions/second');
    const secondManifest = {
      ...manifest, name: '@openclaw/second', dependencies: {value: leafArchive},
    };
    delete secondManifest.bundleDependencies;
    const secondSource = "exports.token={}; exports.direct=require('value'); exports.common=exports.direct; exports.observation={id:'second',generation:1,value:exports.direct.version,peer:require('openclaw/sdk')};\n" + registrationSource;
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
    let expectedSource = treeSnapshot(repo);
    console.log(JSON.stringify({scenario, fixtureSha256: digest(JSON.stringify(expectedSource)), sourceFiles: expectedSource}));
    let native;
    let prerequisiteStep = 'pack';
    try {
      const nativePaths = [];
      for (const id of companionIds) {
        prerequisiteStep = `pack:${id}`;
        const archive = pack(path.join(repo, 'extensions', id));
        const requestedPrefix = path.join(root, `native-${scenario.name}-${id}`);
        fs.mkdirSync(requestedPrefix, {recursive: true});
        // npm's linked-layout control must use one physical prefix spelling.
        // The OCM store deliberately retains its original configured spelling.
        const nativePrefix = scenario.strategy === 'linked'
          ? fs.realpathSync.native(requestedPrefix) : requestedPrefix;
        console.log(JSON.stringify({scenario: scenario.name, phase: 'native-prerequisite', id,
          requestedPrefix, nativePrefix, realpath: fs.realpathSync(requestedPrefix),
          nativeRealpath: fs.realpathSync.native(requestedPrefix),
          configuredOcmHome: env.OCM_HOME, ocmHomeRealpath: fs.realpathSync(env.OCM_HOME),
          ocmHomeNativeRealpath: fs.realpathSync.native(env.OCM_HOME)}));
        prerequisiteStep = `install:${id}`;
        npm(['install', '--prefix', nativePrefix, '--omit=dev', '--omit=peer', '--no-save',
          '--package-lock=false', archive, ...(hostArchive ? [hostArchive] : [])], {env: selectedEnv});
        nativePaths.push(path.join(nativePrefix, 'node_modules/@openclaw', id));
      }
      prerequisiteStep = 'import';
      native = JSON.parse(run(process.execPath,
        ['-e', inspectCompanions + '\ninspect(process.argv.slice(1));', ...nativePaths],
        {env: selectedEnv}));
    } catch (error) {
      console.error(error);
      const failure = {scenario: scenario.name, phase: 'native-prerequisite',
        step: prerequisiteStep, failure: String(error)};
      failures.push(failure);
      console.log(JSON.stringify(failure));
      try {
        assert.deepEqual(treeSnapshot(repo), expectedSource, 'Native preparation changed the source fixture');
        console.log(JSON.stringify({scenario: scenario.name, sourceBytesUnchanged: true}));
      } catch (sourceError) {
        failures.push({scenario: scenario.name, phase: 'native-source-preservation',
          failure: String(sourceError)});
      }
      console.log(JSON.stringify({scenario: scenario.name, elapsedMs: Date.now() - startedAt}));
      continue;
    }
    let phase = 'native-graph';
    try {
      console.log(JSON.stringify({scenario, phase, native}));
      assert.deepEqual(native.companions[0].values, [1, 1, 1]);
      assert.deepEqual(native.companions.map(plugin => plugin.id), companionIds);
      if (scenario.bundled) {
        if (scenario.strategy === 'hoisted') {
          assert.equal(native.companions[0].shared, true, 'Native bridges must share the hoisted module');
          assert.equal(native.companions[0].private, true, 'Native bundled and hoisted modules must remain distinct');
          assert.equal(native.companions[0].cycle, true, 'Native dependency cycles must return the owning companion');
        }
        assert.equal(native.companions[1].value, 1);
        assert.equal(native.isolated, true, 'Independent native companion prefixes must retain separate identity');
        for (const plugin of native.companions) assert.equal(plugin.peer, 'host-sdk-1');
      }
      phase = 'ocm-build';
      const buildArgs = ['runtime', 'build-local', scenario.name, '--repo', repo,
        ...companionIds.flatMap(id => ['--companion', id]), '--json'];
      const metadata = JSON.parse(run(binary, buildArgs, {env: selectedEnv}));
      const installed = path.resolve(metadata.installRoot);
      assert.equal(fs.realpathSync(installed).toLowerCase(),
        fs.realpathSync(path.join(env.OCM_HOME, 'runtimes', scenario.name)).toLowerCase());
      assertNoRuntimeScratch(scenario.name, installed);
      const metadataPath = path.join(env.OCM_HOME, 'runtimes', `${scenario.name}.json`);
      let bound = false;
      function observeRuntime(currentMetadata, expected, observationPhase) {
        phase = `ocm-${observationPhase}`;
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
        const comparisons = [];
        // Do not let a failed admission fixture prevent the production reader
        // from observing the same installed bytes through its own admission.
        try {
          const materialized = JSON.parse(bound
            ? run(binary, [`@${scenario.name}`, '--', '--version'], {env: selectedEnv})
            : run(process.execPath, [currentMetadata.binaryPath, '--version'], {env: selectedEnv}));
          console.log(JSON.stringify({scenario, phase, reader: 'canonical-entry-fixture', native, expected, materialized}));
          assert.deepEqual(materialized, expected, 'Raw import changed the native dependency graph');
          comparisons.push({reader: 'canonical-entry-fixture', status: 'passed'});
        } catch (error) {
          console.error(error);
          comparisons.push({reader: 'canonical-entry-fixture', status: 'failed', failure: String(error)});
        }
        if (openclawSource) {
          try {
            assert.ok(path.isAbsolute(openclawSource), 'OCM_TEST_OPENCLAW_SOURCE must be absolute');
            const readerRequest = {schemaVersion: 1, scenario: scenario.name, phase: observationPhase,
              installRoot: installed, entrypoint: currentMetadata.binaryPath,
              pluginIds: companionIds, expected};
            const result = observe(process.execPath,
              ['--import', pathToFileURL(readerScript).href, currentMetadata.binaryPath, '--version'],
              {env: {...selectedEnv, OCM_TEST_OPENCLAW_SOURCE: openclawSource,
                OCM_TEST_COMPANION_READER_REQUEST: JSON.stringify(readerRequest)}});
            const receipts = result.stdout.split(/\r?\n/).flatMap(line => {
              try {
                const value = JSON.parse(line);
                return value?.actualOpenClawReader === true ? [value] : [];
              } catch {
                return [];
              }
            });
            assert.equal(receipts.length, 1, 'Expected one tagged actual-loader receipt');
            assert.equal(result.status, 0, 'Actual OpenClaw reader process failed');
            assert.equal(receipts[0].status, 'passed');
            assert.deepEqual(receipts[0].graph, expected, 'Actual OpenClaw reader changed the native graph');
            comparisons.push({reader: 'actual-openclaw-loader', status: 'passed'});
          } catch (error) {
            console.error(error);
            comparisons.push({reader: 'actual-openclaw-loader', status: 'failed', failure: String(error)});
          }
        }
        console.log(JSON.stringify({scenario: scenario.name, phase, comparisons,
          actualReaderRequested: Boolean(openclawSource)}));
        assert.equal(fileDigest(metadataPath), metadataSha256, 'Product observations rewrote generated runtime metadata');
        assert.deepEqual(treeSnapshot(repo), expectedSource, 'OCM changed the source fixture');
        assert.deepEqual(comparisons.filter(comparison => comparison.status !== 'passed'), [],
          'Companion reader comparisons failed');
        // Record links without following directory cycles; every resolved target
        // must now belong to the published runtime, never its removed stage.
        return treeSnapshot(installed, true);
      }
      if (!scenario.lifecycle) {
        run(binary, ['env', 'create', scenario.name, '--runtime', scenario.name], {env: selectedEnv});
        bound = true;
      }
      const initialTree = observeRuntime(metadata, native, 'initial');
      if (scenario.bundled) {
        assert.ok(Object.values(initialTree).some(entry => entry.link),
          'The conflicting graph must retain its shared packages through directory bindings');
      }
      // Reuse the hoisted case's replacement/rollback proof; linked cases add
      // their distinct installed graph without multiplying lifecycle builds.
      if (scenario.lifecycle) {
        // Keep the runtime unbound: --force correctly refuses a bound runtime.
        // New code and host SDK markers distinguish replacement from retaining
        // the old tree, without changing the dependency graph under comparison.
        fs.writeFileSync(path.join(companion, 'index.cjs'), companionSource + 'exports.observation.generation=2;\n');
        fs.writeFileSync(path.join(second, 'index.cjs'), secondSource + 'exports.observation.generation=2;\n');
        fs.writeFileSync(path.join(repo, 'sdk.cjs'), "module.exports='host-sdk-2';\n");
        expectedSource = treeSnapshot(repo);
        phase = 'ocm-replacement-build';
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
        phase = 'ocm-failed-replacement';
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
      failures.push({scenario: scenario.name, phase, failure: String(error)});
    } finally {
      try {
        assert.deepEqual(treeSnapshot(repo), expectedSource, 'OCM changed the source fixture');
        console.log(JSON.stringify({scenario: scenario.name, sourceBytesUnchanged: true}));
      } catch (sourceError) {
        failures.push({scenario: scenario.name, phase: 'source-preservation', failure: String(sourceError)});
      }
      console.log(JSON.stringify({scenario: scenario.name, elapsedMs: Date.now() - startedAt}));
    }
  }
  console.log(JSON.stringify({nativeWindowsCompanionObservationsComplete: true,
    selectedCases: selectedNames, allCases: selectedCases.length === cases.length, failures}));
  assert.deepEqual(failures, [], 'Native companion dependency checks failed');
  console.log(JSON.stringify({nativeWindowsCompanionWithoutSymlinkPrivilege: 'passed',
    selectedCases: selectedNames, allCases: selectedCases.length === cases.length}));
}

try {
  main();
} catch (error) {
  console.error(error);
  process.exitCode = 1;
}
