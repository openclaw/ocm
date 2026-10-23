// Public synthetic TLS material reused from OpenClaw test/helpers/tls-fixture.ts.
// This CN-only self-signed leaf intentionally exercises native Gateway pinning,
// including certificate lifetimes independently of the wall clock. Never use it
// for a real service. The second public leaf is a mismatch control from
// OpenClaw src/test-helpers/proxy-tls-fixture.ts.
const fs = require("node:fs");
const http = require("node:http");
const https = require("node:https");
const net = require("node:net");
const PRIVATE_KEY_LABEL = "PRIVATE KEY";

const TEST_TLS_KEY_PEM = [
  `-----BEGIN ${PRIVATE_KEY_LABEL}-----`, // pragma: allowlist secret
  "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDrur5CWp4psMMb",
  "DTPY1aN46HPDxRchGgh8XedNkrlc4z1KFiyLUsXpVIhuyoXq1fflpTDz7++pGEDJ",
  "Q5pEdChn3fuWgi7gC+pvd5VQ1eAX/7qVE72fhx14NxhaiZU3hCzXjG2SflTEEExk",
  "UkQTm0rdHSjgLVMhTM3Pqm6Kzfdgtm9ZyXwlAsorE/pvgbUxG3Q4xKNBGzbirZ+1",
  "EzPDwsjf3fitNtakZJkymu6Kg5lsUihQVXOP0U7f989FmevoTMvJmkvJzsoTRd7s",
  "XNSOjzOwJr8da8C4HkXi21md1yEccyW0iSh7tWvDrpWDAgW6RMuMHC0tW4bkpDGr",
  "FpbQOgzVAgMBAAECggEAIMhwf8Ve9CDVTWyNXpU9fgnj2aDOCeg3MGaVzaO/XCPt",
  "KOHDEaAyDnRXYgMP0zwtFNafo3klnSBWmDbq3CTEXseQHtsdfkKh+J0KmrqXxval",
  "YeikKSyvBEIzRJoYMqeS3eo1bddcXgT/Pr9zIL/qzivpPJ4JDttBzyTeaTbiNaR9",
  "KphGNueo+MTQMLreMqw5VAyJ44gy7Z/2TMiMEc/d95wfubcOSsrIfpOKnMvWd/rl",
  "vxIS33s95L7CjREkixskj5Yo5Wpt3Yf5b0Zi70YiEsCfAZUDrPW7YzMlylzmhMzm",
  "MARZKfN1Tmo74SGpxUrBury+iPwf1sYcRnsHR+zO8QKBgQD6ISQHRzPboZ3J/60+",
  "fRLETtrBa9WkvaH9c+woF7l47D4DIlvlv9D3N1KGkUmhMnp2jNKLIlalBNDxBdB+",
  "iwZP1kikGz4629Ch3/KF/VYscLTlAQNPE42jOo7Hj7VrdQx9zQrK9ZBLteXmSvOh",
  "bB3aXwXPF3HoTMt9gQ9thhXZJQKBgQDxQxUnQSw43dRlqYOHzPUEwnJkGkuW/qxn",
  "aRc8eopP5zUaebiDFmqhY36x2Wd+HnXrzufy2o4jkXkWTau8Ns+OLhnIG3PIU9L/",
  "LYzJMckGb75QYiK1YKMUUSQzlNCS8+TFVCTAvG2u2zCCk7oTIe8aT516BQNjWDjK",
  "gWo2f87N8QKBgHoVANO4kfwJxszXyMPuIeHEpwquyijNEap2EPaEldcKXz4CYB4j",
  "4Cc5TkM12F0gGRuRohWcnfOPBTgOYXPSATOoX+4RCe+KaCsJ9gIl4xBvtirrsqS+",
  "42ue4h9O6fpXt9AS6sii0FnTnzEmtgC8l1mE9X3dcJA0I0HPYytOvY0tAoGAAYJj",
  "7Xzw4+IvY/ttgTn9BmyY/ptTgbxSI8t6g7xYhStzH5lHWDqZrCzNLBuqFBXosvL2",
  "bISFgx9z3Hnb6y+EmOUc8C2LyeMMXOBSEygmk827KRGUGgJiwsvHKDN0Ipc4BSwD",
  "ltkW7pMceJSoA1qg/k8lMxA49zQkFtA8c97U0mECgYEAk2DDN78sRQI8RpSECJWy",
  "l1O1ikVUAYVeh5HdZkpt++ddfpo695Op9OeD2Eq27Y5EVj8Xl58GFxNk0egLUnYq",
  "YzSbjcNkR2SbVvuLaV1zlQKm6M5rfvhj4//YrzrrPUQda7Q4eR0as/3q91uzAO2O",
  "++pfnSCVCyp/TxSkhEDEawU=",
  `-----END ${PRIVATE_KEY_LABEL}-----`,
].join("\n");

const TEST_TLS_CERT_PEM = `-----BEGIN CERTIFICATE-----
MIIDCTCCAfGgAwIBAgIUel0Lv05cjrViyI/H3tABBJxM7NgwDQYJKoZIhvcNAQEL
BQAwFDESMBAGA1UEAwwJbG9jYWxob3N0MB4XDTI2MDEyMDEyMjEzMloXDTI2MDEy
MTEyMjEzMlowFDESMBAGA1UEAwwJbG9jYWxob3N0MIIBIjANBgkqhkiG9w0BAQEF
AAOCAQ8AMIIBCgKCAQEA67q+QlqeKbDDGw0z2NWjeOhzw8UXIRoIfF3nTZK5XOM9
ShYsi1LF6VSIbsqF6tX35aUw8+/vqRhAyUOaRHQoZ937loIu4Avqb3eVUNXgF/+6
lRO9n4cdeDcYWomVN4Qs14xtkn5UxBBMZFJEE5tK3R0o4C1TIUzNz6puis33YLZv
Wcl8JQLKKxP6b4G1MRt0OMSjQRs24q2ftRMzw8LI3934rTbWpGSZMpruioOZbFIo
UFVzj9FO3/fPRZnr6EzLyZpLyc7KE0Xe7FzUjo8zsCa/HWvAuB5F4ttZndchHHMl
tIkoe7Vrw66VgwIFukTLjBwtLVuG5KQxqxaW0DoM1QIDAQABo1MwUTAdBgNVHQ4E
FgQUwNdNkEQtd0n/aofzN7/EeYPPPbIwHwYDVR0jBBgwFoAUwNdNkEQtd0n/aofz
N7/EeYPPPbIwDwYDVR0TAQH/BAUwAwEB/zANBgkqhkiG9w0BAQsFAAOCAQEAnOnw
o8Az/bL0A6bGHTYra3L9ArIIljMajT6KDHxylR4LhliuVNAznnhP3UkcZbUdjqjp
MNOM0lej2pNioondtQdXUskZtqWy6+dLbTm1RYQh1lbCCZQ26o7o/oENzjPksLAb
jRM47DYxRweTyRWQ5t9wvg/xL0Yi1tWq4u4FCNZlBMgdwAEnXNwVWTzRR9RHwy20
lmUzM8uQ/p42bk4EvPEV4PI1h5G0khQ6x9CtkadCTDs/ZqoUaJMwZBIDSrdJJSLw
4Vh8Lqzia1CFB4um9J4S1Gm/VZMBjjeGGBJk7VSYn4ZmhPlbPM+6z39lpQGEG0x4
r1USnb+wUdA7Zoj/mQ==
-----END CERTIFICATE-----`;

const OTHER_CERT = `-----BEGIN CERTIFICATE-----
MIIBsDCCAVagAwIBAgIUYQmcXrXXGwb7iM9LvhwniX+tYY8wCgYIKoZIzj0EAwIw
GzEZMBcGA1UEAwwQZmlsZXMucHJveHkudGVzdDAgFw0yNjA4MjgwMDM2NDlaGA8y
MTI2MDgwNDAwMzY0OVowGzEZMBcGA1UEAwwQZmlsZXMucHJveHkudGVzdDBZMBMG
ByqGSM49AgEGCCqGSM49AwEHA0IABLZqBKM03chE4ezcYB0l3E7SyeaqBbZF624k
p5Fuf97TzZNHxLHn/I9wBzICnqEQfX2De0X8JPv9AaIab3oFmF6jdjB0MB0GA1Ud
DgQWBBRQmY8ZLuaAJjUU4JiehaVg+f+KoDAfBgNVHSMEGDAWgBRQmY8ZLuaAJjUU
4JiehaVg+f+KoDAPBgNVHRMBAf8EBTADAQH/MCEGA1UdEQQaMBiCEGZpbGVzLnBy
b3h5LnRlc3SHBH8AAAEwCgYIKoZIzj0EAwIDSAAwRQIhAL9IXiyh36hCytGAMVBn
6/BdZ4GB+I9sJxihRSip4G+wAiAsSHjAkfx+Yn8VgTrjXWOF9dGhzxnD/kWC4BMK
j3DhDg==
-----END CERTIFICATE-----`;
if (process.argv[2] === "--certificate") {
  process.stdout.write(TEST_TLS_CERT_PEM);
} else if (process.argv[2] === "--other-certificate") {
  process.stdout.write(OTHER_CERT);
} else {
  const [mode, log] = process.argv.slice(2);
  const parent = process.ppid;
  // Every peer is finite even if a failing test interrupts supervisor cleanup.
  setTimeout(() => process.exit(0), 45000).unref();
  setInterval(() => {
    try { process.kill(parent, 0); } catch { process.exit(0); }
  }, 100).unref();
  let activeServer;
  let switched = false;
  const port = Number(process.env.OPENCLAW_GATEWAY_PORT);
  const host = process.env.READINESS_BIND_HOST || "127.0.0.1";
  const configureTimeouts = server => {
    server.requestTimeout = 2000;
    server.headersTimeout = 2000;
    server.on("connection", socket => socket.setTimeout(2000, () => socket.destroy()));
  };
  const handle = (request, response) => {
    fs.appendFileSync(log, JSON.stringify({pid: process.pid, path: request.url,
      host: request.headers.host, tls: !!request.socket.encrypted}) + "\n");
    if (!switched && request.socket.encrypted &&
        (mode === "tls-then-missing" || mode === "tls-then-http-config")) {
      switched = true;
      // Stop accepting TLS while retaining this authenticated connection. Bind
      // HTTP on the released listener before delivering its failing response, so
      // the next readiness poll cannot miss the downgrade opportunity by racing.
      activeServer.close();
      activeServer = http.createServer(handle);
      configureTimeouts(activeServer);
      activeServer.listen(port, "127.0.0.1", () => {
        const configPath = process.env.OPENCLAW_CONFIG_PATH;
        if (mode === "tls-then-missing") {
          fs.unlinkSync(configPath);
        } else {
          const config = JSON.parse(fs.readFileSync(configPath, "utf8"));
          config.gateway.tls.enabled = false;
          fs.writeFileSync(configPath, JSON.stringify(config));
        }
        fs.writeFileSync(log + ".switched", mode);
        response.writeHead(503);
        response.end("{\"ok\":false}");
      });
      return;
    }
    response.writeHead(mode === "unhealthy-tls" ? 503 : 200);
    response.end("{\"ok\":true}");
  };
  if (mode === "unreachable") {
    setInterval(() => {}, 1000);
  } else if (mode === "silent-tls") {
    net.createServer(socket => socket.setTimeout(5000, () => socket.destroy()))
      .listen(Number(process.env.OPENCLAW_GATEWAY_PORT), "127.0.0.1");
  } else {
    const plain = mode === "http" || mode === "plaintext-only";
    const server = plain ? http.createServer(handle) : https.createServer({
      cert: TEST_TLS_CERT_PEM, key: TEST_TLS_KEY_PEM,
      minVersion: mode === "tls12" ? "TLSv1.2" : "TLSv1.3",
      maxVersion: mode === "tls12" ? "TLSv1.2" : "TLSv1.3",
    }, handle);
    activeServer = server;
    configureTimeouts(server);
    server.listen(port, host, () => {
      fs.writeFileSync(log + ".listening", JSON.stringify({pid: process.pid, tls: !plain}));
    });
  }
}
