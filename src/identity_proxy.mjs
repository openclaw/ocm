#!/usr/bin/env node

import { execFile } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import net from "node:net";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const STRIPPED_HEADERS = new Set([
  "forwarded",
  "x-forwarded-for",
  "x-forwarded-host",
  "x-forwarded-proto",
  "x-real-ip",
  "x-openclaw-proxy",
  "x-openclaw-user",
  "tailscale-user-login",
  "tailscale-user-name",
  "tailscale-user-profile-pic",
  "content-length",
]);

function extractForwardedIp(value) {
  const first = Array.isArray(value) ? value[0] : value;
  if (typeof first !== "string") {
    return null;
  }
  const candidate = first.split(",", 1)[0]?.trim();
  if (!candidate) {
    return null;
  }
  if (candidate.startsWith("[") && candidate.includes("]")) {
    return candidate.slice(1, candidate.indexOf("]"));
  }
  const ipv4WithPort = candidate.match(/^(\d{1,3}(?:\.\d{1,3}){3}):\d+$/);
  return ipv4WithPort?.[1] ?? candidate;
}

function sanitizeHeaders(headers, identity, clientIp) {
  const result = {};
  for (const [name, value] of Object.entries(headers)) {
    const lower = name.toLowerCase();
    if (STRIPPED_HEADERS.has(lower) || lower.startsWith("tailscale-")) {
      continue;
    }
    if (value !== undefined) {
      result[lower] = value;
    }
  }
  result["x-forwarded-for"] = clientIp;
  result["x-forwarded-host"] =
    headers["x-forwarded-host"] ?? headers.host ?? "localhost";
  result["x-forwarded-proto"] = "https";
  result["x-openclaw-proxy"] = "ocm-tailscale-identity-v1";
  result["x-openclaw-user"] = identity.login;
  return result;
}

function parseJsonOutput(stdout) {
  const start = stdout.indexOf("{");
  if (start < 0) {
    throw new Error("tailscale whois returned no JSON");
  }
  return JSON.parse(stdout.slice(start));
}

function createWhoisResolver({ endpoints, cacheTtlMs = 300_000 }) {
  const cache = new Map();
  return async (ip) => {
    const cached = cache.get(ip);
    if (cached && cached.expiresAt > Date.now()) {
      return cached.identity;
    }
    let lastError;
    for (const endpoint of endpoints) {
      const args = [];
      if (endpoint.socket) {
        args.push(`--socket=${endpoint.socket}`);
      }
      args.push("whois", "--json", ip);
      try {
        const { stdout } = await execFileAsync(endpoint.binary, args, {
          timeout: 5_000,
          maxBuffer: 1024 * 1024,
        });
        const parsed = parseJsonOutput(stdout);
        const login = parsed?.UserProfile?.LoginName?.trim();
        if (!login) {
          throw new Error("tailscale whois returned no user login");
        }
        const identity = { login };
        cache.set(ip, { identity, expiresAt: Date.now() + cacheTtlMs });
        return identity;
      } catch (error) {
        lastError = error;
      }
    }
    throw lastError ?? new Error("no Tailscale identity endpoint accepted the client IP");
  };
}

function writeSocketError(socket, status, message) {
  if (socket.destroyed) {
    return;
  }
  const body = `${message}\n`;
  socket.end(
    `HTTP/1.1 ${status}\r\n` +
      "Connection: close\r\n" +
      "Content-Type: text/plain; charset=utf-8\r\n" +
      `Content-Length: ${Buffer.byteLength(body)}\r\n\r\n` +
      body,
  );
}

async function resolveRequestIdentity(req, resolveIdentity) {
  const remote = req.socket.remoteAddress?.replace(/^::ffff:/, "") ?? "";
  if (remote !== "127.0.0.1" && remote !== "::1") {
    throw new Error("identity proxy only accepts loopback callers");
  }
  if (req.headers["x-forwarded-proto"] !== "https" || !req.headers["x-forwarded-host"]) {
    throw new Error("missing Tailscale Serve forwarding headers");
  }
  const ip = extractForwardedIp(req.headers["x-forwarded-for"]);
  if (!ip || net.isIP(ip) === 0) {
    throw new Error("missing or invalid forwarded Tailscale client IP");
  }
  return { ip, identity: await resolveIdentity(ip) };
}

function createIdentityProxy({ config, logger = console }) {
  const resolveIdentity = createWhoisResolver({ endpoints: config.tailscaleEndpoints });
  const server = http.createServer(async (req, res) => {
    try {
      const { ip, identity } = await resolveRequestIdentity(req, resolveIdentity);
      const headers = sanitizeHeaders(req.headers, identity, ip);
      const upstream = http.request(
        {
          host: config.upstreamHost,
          port: config.upstreamPort,
          method: req.method,
          path: req.url,
          headers,
        },
        (upstreamRes) => {
          res.writeHead(upstreamRes.statusCode ?? 502, upstreamRes.headers);
          upstreamRes.pipe(res);
        },
      );
      upstream.on("error", (error) => {
        logger.error(JSON.stringify({ event: "upstream_error", error: error.message }));
        if (!res.headersSent) {
          res.writeHead(502, { "content-type": "text/plain; charset=utf-8" });
        }
        res.end("OpenClaw gateway unavailable\n");
      });
      req.pipe(upstream);
    } catch (error) {
      logger.warn(JSON.stringify({ event: "auth_rejected", error: error.message }));
      res.writeHead(403, { "content-type": "text/plain; charset=utf-8" });
      res.end("Tailscale identity required\n");
    }
  });

  server.on("upgrade", async (req, socket, head) => {
    try {
      const { ip, identity } = await resolveRequestIdentity(req, resolveIdentity);
      const headers = sanitizeHeaders(req.headers, identity, ip);
      headers.connection = "Upgrade";
      headers.upgrade = req.headers.upgrade ?? "websocket";
      const upstream = net.connect({
        host: config.upstreamHost,
        port: config.upstreamPort,
      });
      upstream.on("connect", () => {
        let request = `${req.method ?? "GET"} ${req.url ?? "/"} HTTP/${req.httpVersion}\r\n`;
        for (const [name, value] of Object.entries(headers)) {
          if (Array.isArray(value)) {
            for (const item of value) {
              request += `${name}: ${item}\r\n`;
            }
          } else if (value !== undefined) {
            request += `${name}: ${value}\r\n`;
          }
        }
        upstream.write(`${request}\r\n`);
        if (head.length > 0) {
          upstream.write(head);
        }
        socket.pipe(upstream).pipe(socket);
      });
      upstream.on("error", (error) => {
        logger.error(JSON.stringify({ event: "upstream_upgrade_error", error: error.message }));
        writeSocketError(socket, "502 Bad Gateway", "OpenClaw gateway unavailable");
      });
      socket.on("error", () => upstream.destroy());
    } catch (error) {
      logger.warn(JSON.stringify({ event: "upgrade_auth_rejected", error: error.message }));
      writeSocketError(socket, "403 Forbidden", "Tailscale identity required");
    }
  });

  return server;
}

const configPath = process.argv[2];
if (!configPath) {
  throw new Error("identity proxy requires a config path");
}
const config = JSON.parse(fs.readFileSync(configPath, "utf8"));
const server = createIdentityProxy({ config });
server.listen(config.listenPort, config.listenHost, () => {
  console.log(
    JSON.stringify({
      event: "identity_proxy_ready",
      listen: `${config.listenHost}:${config.listenPort}`,
      upstream: `${config.upstreamHost}:${config.upstreamPort}`,
    }),
  );
});
