const fs = require("node:fs");
const http = require("node:http");
const path = require("node:path");
const {spawn} = require("node:child_process");

const root = process.env.OCM_TEST_DEV_UI_DIR;
const file = (name) => path.join(root, name);
const exists = (name) => fs.existsSync(file(name));
const config = JSON.parse(fs.readFileSync(process.env.OPENCLAW_CONFIG_PATH, "utf8"));
const controlUi = config.gateway.controlUi?.$include
  ? JSON.parse(fs.readFileSync(path.resolve(path.dirname(process.env.OPENCLAW_CONFIG_PATH), config.gateway.controlUi.$include), "utf8"))
  : config.gateway.controlUi;
// Controlled peers implement only the config inputs used by these CLI tests.
// Native OpenClaw remains the authority for its complete config/env semantics.
const nativeEnv = {
  UI_BASE: process.env.UI_BASE,
  OCM_TEST_UI_BASE: process.env.OCM_TEST_UI_BASE,
  OCM_ACTIVE_ENV: process.env.OCM_ACTIVE_ENV,
  OPENCLAW_GATEWAY_PORT: process.env.OPENCLAW_GATEWAY_PORT,
};
for (const dotenv of [path.join(process.cwd(), ".env"), path.join(path.dirname(process.env.OPENCLAW_CONFIG_PATH), ".env")]) {
  if (!fs.existsSync(dotenv)) continue;
  for (const line of fs.readFileSync(dotenv, "utf8").split(/\r?\n/)) {
    const entry = /^([A-Z_][A-Z0-9_]*)=(.*)$/.exec(line);
    if (entry && Object.hasOwn(nativeEnv, entry[1]) && nativeEnv[entry[1]] === undefined) nativeEnv[entry[1]] = entry[2];
  }
}
for (const [key, value] of Object.entries({...config.env?.vars, ...config.env})) {
  if (Object.hasOwn(nativeEnv, key) && typeof value === "string" && value.trim() && !value.includes("${") && !nativeEnv[key]?.trim()) {
    nativeEnv[key] = value;
  }
}
const resolvedBase = String(controlUi?.basePath ?? "").replace(/\$\$?\{([A-Z_][A-Z0-9_]*)\}/g,
  (reference, name) => reference.startsWith("$$") ? reference.slice(1) : nativeEnv[name] || reference);
const gatewayPort = Number(process.env.OPENCLAW_GATEWAY_PORT);
const base = resolvedBase.replace(/^\/+|\/+$/g, "");
const documentPath = base ? `/${base}/` : "/";
const gatewayUrl = `http://127.0.0.1:${gatewayPort}${documentPath}`;
const role = process.argv.includes("config")
  ? "config"
  : process.argv.includes("dashboard")
  ? "dashboard"
  : process.argv[1].endsWith("ui.js")
    ? "ui"
    : "gateway";

if (role === "config") {
  if (JSON.stringify(process.argv.slice(2)) !== JSON.stringify(["config", "get", "gateway.controlUi.basePath", "--json"])) {
    throw new Error("unexpected native config read");
  }
  fs.appendFileSync(file("config-attempts"), `${process.pid}\n`);
  process.stderr.write("synthetic-private-config-diagnostic\n");
  process.once("SIGTERM", () => process.exit(143));
  const timer = setInterval(() => {
    if (exists("source-watch.release")) process.exit(0);
    if (exists("config-hold")) return;
    clearInterval(timer);
    if (exists("config-fail")) {
      process.stdout.write("synthetic-private-config-output\n");
      process.exit(7);
    }
    if (exists("config-malformed")) {
      process.stdout.write("synthetic-private-config-output\n");
    } else {
      console.log(JSON.stringify(resolvedBase));
    }
  }, 20);
} else if (role === "dashboard") {
  const attempts = file("dashboard-attempts");
  fs.appendFileSync(attempts, `${process.pid}\n`);
  const count = fs.readFileSync(attempts, "utf8").trim().split("\n").length;
  fs.writeFileSync(file(`dashboard-attempt-${count}`), String(process.pid));
  const deliver = () => {
    if (exists("dashboard-pending")) {
      console.log(JSON.stringify({ ok: false, reason: "fixture Gateway is pending" }));
      process.exit(1);
    }
    const link = new URL(gatewayUrl);
    link.hash = new URLSearchParams({
      bootstrapToken: `synthetic-owner-grant-${count}`,
      bootstrapProfile: "control-ui-owner",
      gatewayUrl: gatewayUrl.replace(/^http:/, "ws:"),
    }).toString();
    console.log(JSON.stringify({
      ok: true,
      browserUrl: link.toString(),
      browserBootstrapExpiresAtMs: Date.now() + 60000,
      url: `${gatewayUrl}#token=synthetic-legacy-token`,
      gatewayPassword: "synthetic-legacy-password",
    }));
    fs.writeFileSync(file(`dashboard-emitted-${count}`), "ready");
    process.exit(0);
  };
  const timer = setInterval(() => {
    if (exists("source-watch.release")) process.exit(0);
    if (!exists("dashboard-hold")) {
      clearInterval(timer);
      deliver();
    }
  }, 20);
} else {
  const port = role === "ui"
    ? Number(process.argv[process.argv.indexOf("--port") + 1])
    : gatewayPort;
  const descendant = process.env.OCM_TEST_DEV_UI_DESCENDANTS === "1"
    ? spawn(process.execPath, ["-e", "setInterval(()=>{},1000)"], {stdio:"ignore", windowsHide:true})
    : undefined;
  const server = http.createServer((request, response) => {
    fs.appendFileSync(file(`${role}-requests`), `${request.url}\n`);
    const ready = role === "ui"
      || request.url === "/health"
      || (request.url === documentPath && exists("gateway-document-ready"));
    const health = request.url === "/health";
    response.writeHead(ready ? 200 : 503, {
      "content-type": health || !ready ? "application/json" : "text/html; charset=utf-8",
    });
    response.end(health ? '{"ok":true}' : ready ? "<!doctype html><title>OCM UI fixture</title>" : '{"pending":true}');
  });
  // Ordinary native owners acknowledge a handled stop after their resources close.
  let stopping = false;
  process.once("SIGTERM", () => {
    stopping = true;
    server.close(() => process.exit(143));
  });
  let listenDeadline;
  const listen = () => {
    if (stopping) return;
    listenDeadline ??= Date.now() + 5000;
    const onListening = () => {
      server.off("error", onError);
      const temporary = file(`${role}.${process.pid}.tmp`);
      fs.writeFileSync(temporary, JSON.stringify({
        pid: process.pid,
        descendantPid: descendant?.pid,
        port,
        cwd: fs.realpathSync(process.cwd()),
        entrypoint: path.basename(process.argv[1]),
        gatewayUrl: process.env.OPENCLAW_UI_DEV_GATEWAY_URL,
        uiBasePath: process.env.OPENCLAW_CONTROL_UI_BASE_PATH,
        args: process.argv.slice(2),
      }));
      fs.renameSync(temporary, file(`${role}.json`));
    };
    const onError = (error) => {
      server.off("listening", onListening);
      const temporary = file(`${role}.error.${process.pid}.tmp`);
      fs.writeFileSync(temporary, error.code ?? "UNKNOWN");
      fs.renameSync(temporary, file(`${role}-listen-error`));
      // The native Gateway retries transient EADDRINUSE from other processes'
      // availability probes. Keep this fake's retry inside its 10s startup
      // budget; Vite's --strictPort behavior and other errors remain immediate.
      if (role === "gateway" && error.code === "EADDRINUSE" && Date.now() < listenDeadline) {
        server.close(() => setTimeout(listen, 20));
        return;
      }
      throw error;
    };
    server.once("error", onError);
    server.once("listening", onListening);
    server.listen(port, "127.0.0.1");
  };
  const start = setInterval(() => {
    if (!exists(`${role}-start-hold`)) {
      clearInterval(start);
      listen();
    }
  }, 20);
  setInterval(() => {
    if (exists("source-watch.release")) process.exit(0);
    if (exists(`${role}.exit`)) process.exit(17);
  }, 20);
}
