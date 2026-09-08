import http from "node:http";
import { readFile } from "node:fs/promises";
import { resolve, extname } from "node:path";
import { pathToFileURL } from "node:url";
import { createGateway } from "./gateway.mjs";
export function startServer({
  port = Number(process.env.PORT || 4325),
  host = process.env.HOST || "127.0.0.1",
  origin = process.env.WAVEFORM_FRONTEND_ORIGIN || `http://localhost:${port}`,
  backend = process.env.WAVEFORM_BACKEND_URL,
  iam = process.env.WAVEFORM_IAM_AUTH_ORIGIN,
  appId = process.env.WAVEFORM_APP_ID,
} = {}) {
  const gateway = createGateway({ origin, backend, iam, appId });
  const root = resolve(import.meta.dirname, "../dist");
  const types = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript",
    ".css": "text/css",
    ".svg": "image/svg+xml",
    ".woff2": "font/woff2",
  };
  const server = http.createServer(async (req, res) => {
    try {
      const url = new URL(req.url, origin);
      if (/^\/(api|auth|health)\//.test(url.pathname)) {
        const chunks = [];
        let size = 0;
        for await (const chunk of req) {
          size += chunk.length;
          if (size > 128 * 1024) {
            res.writeHead(413);
            res.end();
            return;
          }
          chunks.push(chunk);
        }
        const request = new Request(url, {
          method: req.method,
          headers: req.headers,
          body: ["GET", "HEAD"].includes(req.method)
            ? undefined
            : Buffer.concat(chunks),
        });
        const response = await gateway(request);
        const bytes = Buffer.from(await response.arrayBuffer());
        res.writeHead(response.status, Object.fromEntries(response.headers));
        res.end(bytes);
        return;
      }
      if (!["GET", "HEAD"].includes(req.method)) {
        res.writeHead(405);
        res.end();
        return;
      }
      const pathname = decodeURIComponent(url.pathname);
      let path = resolve(root, "." + pathname);
      if (!path.startsWith(root + "/")) path = resolve(root, "index.html");
      let data;
      try {
        data = await readFile(path);
      } catch {
        if (extname(pathname)) {
          res.writeHead(404);
          res.end();
          return;
        }
        path = resolve(root, "index.html");
        data = await readFile(path);
      }
      res.writeHead(200, {
        "content-type": types[extname(path)] || "application/octet-stream",
        "cache-control": path.includes("/assets/")
          ? "public, max-age=31536000, immutable"
          : "no-cache",
        "referrer-policy": "no-referrer",
        "x-content-type-options": "nosniff",
        "content-security-policy":
          "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; font-src 'self'; img-src 'self' data:; media-src 'self' https: blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
      });
      res.end(req.method === "HEAD" ? undefined : data);
    } catch {
      res.writeHead(500, { "content-type": "text/plain" });
      res.end("Unable to serve this request.");
    }
  });
  server.requestTimeout = 300_000;
  server.listen(port, host, () =>
    console.log(`Waveform frontend listening at ${origin}`),
  );
  return server;
}
if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
)
  startServer();
