// Installed-package capability fixture. It models only the bounded native
// config command contract and delegates Gateway HTTP(S) to the finite peer.
import fs from 'node:fs';
import path from 'node:path';
import {parseEnv} from 'node:util';
import {pathToFileURL} from 'node:url';
import {spawn} from 'node:child_process';

// Deliberately finite fixture contract: JSON root plus at most two ordered root includes.
// Authorized external roots come from native dotenv or the actual launcher env;
// the OCM caller does not receive this fixture's ambient include authority.
function fixtureGateway() {
  const configPath = process.env.OPENCLAW_CONFIG_PATH;
  const configDirectory = path.dirname(configPath);
  const dotenv = path.join(configDirectory, '.env');
  const values = fs.existsSync(dotenv) ? parseEnv(fs.readFileSync(dotenv, 'utf8')) : {};
  const environment = {...values, ...process.env};
  let config = JSON.parse(fs.readFileSync(configPath, 'utf8'));
  if (config.$include) {
    const includes = Array.isArray(config.$include) ? config.$include : [config.$include];
    if (includes.length < 1 || includes.length > 2) throw new Error('fixture include count unsupported');
    const roots = [configDirectory, ...(environment.OPENCLAW_INCLUDE_ROOTS ?? '').split(path.delimiter).filter(Boolean)];
    config = {};
    for (const include of includes) {
      const included = fs.realpathSync(path.resolve(configDirectory, include));
      const permitted = roots.some(root => {
        const relative = path.relative(fs.realpathSync(root), included);
        return relative !== '..' && !relative.startsWith('..' + path.sep) && !path.isAbsolute(relative);
      });
      if (!permitted || fs.statSync(included).size > 65536) throw new Error('fixture include denied');
      const next = JSON.parse(fs.readFileSync(included, 'utf8'));
      if (next.$include) throw new Error('nested fixture include unsupported');
      // Only the object shapes needed by these finite Gateway fixtures. A later
      // TLS setting overrides the earlier leaf without dropping its other fields.
      config = {...config, ...next, gateway: {...config.gateway, ...next.gateway,
        tls: {...config.gateway?.tls, ...next.gateway?.tls}}};
    }
  }
  const gateway = structuredClone(config.gateway);
  if (typeof gateway.tls?.certPath === 'string') {
    gateway.tls.certPath = gateway.tls.certPath.replace(/\$\{([^}]+)\}/g,
      (_, name) => environment[name] ?? '');
  }
  if (typeof gateway.customBindHost === 'string') {
    gateway.customBindHost = gateway.customBindHost.replace(/\$\{([^}]+)\}/g,
      (_, name) => environment[name] ?? '');
  }
  return gateway;
}

if (process.argv[2] === 'config') {
  const blocked = behavior === 'interrupt' && fs.existsSync(queries + '.block');
  let descendantPid;
  if (blocked) {
    // Inherit both capture pipes, so killing just the direct helper is insufficient.
    const descendant = spawn(process.execPath, ['-e',
      `require('node:fs').writeFileSync(${JSON.stringify(queries + '.descendant-ready')}, String(process.pid));
       process.stdout.write('NATIVE_CONFIG_PRIVATE_SENTINEL_DESCENDANT');
       setTimeout(() => process.exit(0), 45000);`
    ], {stdio: ['ignore', 'inherit', 'inherit']});
    descendantPid = descendant.pid;
  }
  fs.appendFileSync(queries, JSON.stringify({pid: process.pid, descendantPid, args: process.argv.slice(2)}) + '\n');
  if (behavior === 'failure' || behavior === 'timeout' || blocked) {
    process.stdout.write('NATIVE_CONFIG_PRIVATE_SENTINEL_STDOUT');
    process.stderr.write('NATIVE_CONFIG_PRIVATE_SENTINEL_STDERR');
    if (behavior === 'failure') {
      process.exitCode = 1;
    } else {
      // The OCM lookup must settle this child before its own readiness deadline.
      setTimeout(() => process.exit(0), 45000);
    }
  } else {
    const gateway = fixtureGateway();
    if (behavior === 'invalid-settings') {
      // Successful JSON parsing must not turn malformed native settings into an
      // opaque fallback or expose a value from the private captured output.
      gateway.tls = {enabled: true, certPath: {private: 'NATIVE_CONFIG_PRIVATE_SENTINEL_SETTINGS'}};
    }
    process.stdout.write(JSON.stringify(gateway), () => {
      if (behavior === 'retry-generation') {
        fs.appendFileSync(queries + '.results', JSON.stringify({pid: process.pid,
          certificate: gateway.tls.certPath}) + '\n');
        if (gateway.tls.certPath === 'missing-first.pem') {
          // Publish only after the first result has been fixed and written. The
          // Gateway must not change dotenv early enough to alter this lookup.
          fs.writeFileSync(queries + '.first-result', String(process.pid));
        }
      }
    });
  }
} else if (process.argv[2] === 'gateway' && process.argv[3] === 'run') {
  if (behavior === 'retry-generation') {
    const generations = queries + '.generations';
    const firstGeneration = !fs.existsSync(generations);
    fs.appendFileSync(generations, JSON.stringify({pid: process.pid}) + '\n');
    if (firstGeneration) {
      const waitForFirstResult = setInterval(() => {
        if (!fs.existsSync(queries + '.first-result')) return;
        clearInterval(waitForFirstResult);
        const dotenv = path.join(path.dirname(process.env.OPENCLAW_CONFIG_PATH), '.env');
        const before = fs.readFileSync(dotenv, 'utf8');
        fs.writeFileSync(dotenv, before.replace('missing-first.pem', 'fixture-cert.pem'));
        process.exit(1);
      }, 20);
    }
  }
  const gateway = fixtureGateway();
  if ((gateway.tls?.enabled === true) !== (mode !== 'http')) throw new Error('fixture transport mismatch');
  process.argv = [process.argv[0], peer, mode, requests];
  await import(pathToFileURL(peer).href);
} else if (process.argv[2] === '--version') {
  process.stdout.write('OpenClaw 0.0.0\n');
} else {
  // Capabilities and unrelated commands are not implemented by this finite peer.
  process.exitCode = 2;
}
