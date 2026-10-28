// Fixture-only preload: argv[1] remains the OCM-produced host entry, so the
// production loader owns bundled discovery, final admission and registration.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import {createRequire} from 'node:module';
import path from 'node:path';
import {pathToFileURL} from 'node:url';

const expectedSourceCommit = '8f2190bb63cc8ff1cafceffbf1bb429232c1ad5d';
const captureKey = Symbol.for('ocm.r4.registrations');
const require = createRequire(import.meta.url);
const report = {actualOpenClawReader: true, schemaVersion: 1, expectedSourceCommit,
  status: 'failed', failures: []};
let phase = 'request';
let request;
let registry;
let dispose;
const registrations = [];

function fileDigest(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

function identity(file) {
  const stat = fs.statSync(file, {bigint: true});
  assert.notEqual(stat.ino, 0n, `Missing filesystem identity for ${file}`);
  return {dev: String(stat.dev), ino: String(stat.ino)};
}

function inside(root, file) {
  const relative = path.relative(root, file);
  return !path.isAbsolute(relative) && relative !== '..' && !relative.startsWith(`..${path.sep}`);
}

function moduleDiagnostics(installRoot) {
  const physicalRoot = fs.realpathSync.native(installRoot);
  const identities = new Map();
  const objectId = value => {
    if (!value || !['object', 'function'].includes(typeof value)) return null;
    if (!identities.has(value)) identities.set(value, identities.size + 1);
    return identities.get(value);
  };
  const entries = Object.entries(require.cache).filter(([, entry]) =>
    ['index.cjs', 'sdk.cjs'].includes(path.basename(entry.filename))
    && (inside(installRoot, entry.filename) || inside(physicalRoot, entry.filename)));
  return {modules: entries.slice(0, 32).map(([cacheKey, entry]) => ({
    cacheKey, nativeRealpath: fs.realpathSync.native(entry.filename),
    exportsId: objectId(entry.exports), tokenId: objectId(entry.exports?.token),
    directId: objectId(entry.exports?.direct), commonId: objectId(entry.exports?.common),
    valueId: objectId(entry.exports?.value), pluginId: entry.exports?.observation?.id ?? null,
  })), omittedModules: Math.max(0, entries.length - 32)};
}

function fail(error, failedPhase = phase) {
  report.failures.push({phase: failedPhase, message: String(error),
    stack: typeof error?.stack === 'string' ? error.stack.slice(0, 8000) : undefined});
}

try {
  request = JSON.parse(process.env.OCM_TEST_COMPANION_READER_REQUEST ?? 'null');
  assert.equal(request?.schemaVersion, 1, 'Missing reader request');
  assert.ok(Array.isArray(request.pluginIds) && request.pluginIds.length > 0);
  assert.equal(new Set(request.pluginIds).size, request.pluginIds.length);
  assert.deepEqual(request.expected.companions.map(plugin => plugin.id), request.pluginIds);
  assert.equal(path.resolve(process.argv[1]), path.resolve(request.entrypoint),
    'The reader must preload the emitted host entry without replacing argv[1]');
  const sourceRoot = path.resolve(process.env.OCM_TEST_OPENCLAW_SOURCE ?? '');
  assert.ok(process.env.OCM_TEST_OPENCLAW_SOURCE, 'Pass the prepared pinned OpenClaw source');
  const installed = path.resolve(request.installRoot);
  const bundledRoot = path.join(installed, 'files/node_modules/openclaw/dist/extensions');
  for (const key of ['OPENCLAW_BUNDLED_PLUGINS_DIR', 'OPENCLAW_TEST_TRUST_BUNDLED_PLUGINS_DIR',
    'OPENCLAW_DEV_SOURCE_ROOT']) {
    assert.ok(!process.env[key], `${key} must not replace normal bundled discovery`);
  }
  Object.assign(report, {scenario: request.scenario, observationPhase: request.phase,
    entrypoint: process.argv[1], entrypointNativeRealpath: fs.realpathSync.native(process.argv[1]),
    sourceRoot, bundledRoot, expected: request.expected,
    sourceFiles: Object.fromEntries(['package.json', 'scripts/tsx.mjs', 'src/plugins/loader.ts',
      'src/plugins/loader-runtime-candidate.ts', 'src/plugins/path-safety.ts']
      .map(relative => [relative, fileDigest(path.join(sourceRoot, relative))]))});

  // The job owns the exact Git pin and frozen install. This existing preload
  // selects tsx/esm only; it leaves the fixture's compiled CJS modules native.
  phase = 'source-import';
  await import(pathToFileURL(path.join(sourceRoot, 'scripts/tsx.mjs')).href);
  const {loadOpenClawPlugins} = await import(pathToFileURL(path.join(sourceRoot, 'src/plugins/loader.ts')).href);
  const {resolveBundledPluginsDir} = await import(pathToFileURL(path.join(sourceRoot, 'src/plugins/bundled-dir.ts')).href);
  const {getPluginModuleLoaderStats} = await import(pathToFileURL(path.join(sourceRoot, 'src/plugins/plugin-module-loader-cache.ts')).href);
  ({disposePluginRegistryInstances: dispose} = await import(pathToFileURL(path.join(sourceRoot, 'src/plugins/runtime.ts')).href));
  assert.equal(typeof dispose, 'function');

  phase = 'bundled-discovery';
  const discoveredRoot = resolveBundledPluginsDir(process.env);
  report.discoveredRoot = discoveredRoot;
  assert.ok(discoveredRoot, 'No bundled root was discovered');
  assert.deepEqual(identity(discoveredRoot), identity(bundledRoot),
    'The loader selected a different host bundled root');

  globalThis[captureKey] = registrations;
  const before = getPluginModuleLoaderStats();
  phase = 'runtime-load';
  registry = loadOpenClawPlugins({
    cache: false,
    activate: false,
    onlyPluginIds: request.pluginIds,
    config: {plugins: {allow: request.pluginIds,
      entries: Object.fromEntries(request.pluginIds.map(id => [id, {enabled: true}])),
      slots: {memory: 'none'}}},
  });
  const after = getPluginModuleLoaderStats();
  report.loaderStats = {before, after};
  report.plugins = registry.plugins.map(plugin => ({id: plugin.id, origin: plugin.origin,
    status: plugin.status, rootDir: plugin.rootDir, source: plugin.source, error: plugin.error}));
  report.diagnostics = registry.diagnostics;
  report.registrations = registrations.map(({id, observation, filename}) => ({id, observation,
    filename, realpath: fs.realpathSync(filename), nativeRealpath: fs.realpathSync.native(filename)}));
  const captured = request.pluginIds.map(id => registrations.find(plugin => plugin.id === id));
  if (captured.every(Boolean)) {
    report.graph = {companions: captured.map(plugin => plugin.observation),
      isolated: captured.every((plugin, index) => captured.slice(0, index).every(previous =>
        plugin.token !== previous.token && plugin.direct !== previous.direct && plugin.common !== previous.common))};
  }
  report.moduleIdentity = moduleDiagnostics(installed);

  phase = 'registry';
  assert.deepEqual(registry.plugins.map(plugin => plugin.id).sort(), [...request.pluginIds].sort());
  assert.deepEqual(registrations.map(plugin => plugin.id).sort(), [...request.pluginIds].sort(),
    'Each selected plugin must register exactly once');
  for (const plugin of registry.plugins) {
    assert.equal(plugin.origin, 'bundled');
    assert.equal(plugin.status, 'loaded', plugin.error);
    assert.ok(!plugin.error, plugin.error);
    const expectedRoot = path.join(bundledRoot, plugin.id);
    assert.deepEqual(identity(plugin.rootDir), identity(expectedRoot));
    assert.deepEqual(identity(plugin.source), identity(path.join(expectedRoot, 'index.cjs')));
    const registered = registrations.find(entry => entry.id === plugin.id);
    assert.deepEqual(identity(registered.filename), identity(plugin.source));
  }
  phase = 'graph';
  assert.deepEqual(report.graph, request.expected, 'Actual OpenClaw admission changed the native dependency graph');
  phase = 'native-loader';
  assert.ok(after.nativeHits - before.nativeHits >= request.pluginIds.length,
    'The compiled companion entries did not use the native loader');
  assert.equal(after.sourceTransformFallbacks, before.sourceTransformFallbacks);
  assert.equal(after.sourceTransformForced, before.sourceTransformForced);
} catch (error) {
  fail(error);
  if (request?.installRoot && !report.moduleIdentity) {
    try {
      report.moduleIdentity = moduleDiagnostics(path.resolve(request.installRoot));
    } catch (diagnosticError) {
      report.moduleDiagnosticError = String(diagnosticError);
    }
  }
} finally {
  if (registry && dispose) {
    try {
      await dispose(registry);
    } catch (error) {
      fail(error, 'dispose');
    }
  }
  if (globalThis[captureKey] === registrations) delete globalThis[captureKey];
}

report.status = report.failures.length === 0 ? 'passed' : 'failed';
// The host's later raw-import JSON is a separate control, never this receipt.
console.log(JSON.stringify(report));
if (report.status !== 'passed') {
  throw new Error(`Actual OpenClaw reader proof failed: ${report.failures.map(failure => failure.phase).join(', ')}`);
}
