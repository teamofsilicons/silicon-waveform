// Disposable, local-only UI contract fixture. Never included in the production server.
import http from "node:http";
import { fixture } from "./fixture.mjs";
import { startServer } from "../server/node.mjs";
const fake = fixture();
const backend = http.createServer(async (req, res) => {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  const response = await fake.fetcher(
    new URL(req.url, "http://127.0.0.1:4340"),
    {
      method: req.method,
      headers: req.headers,
      body: chunks.length ? Buffer.concat(chunks).toString() : undefined,
    },
  );
  res.writeHead(response.status, Object.fromEntries(response.headers));
  res.end(Buffer.from(await response.arrayBuffer()));
});
backend.listen(4340, "127.0.0.1");
const server = startServer({
  port: 4341,
  origin: "http://localhost:4341",
  backend: "http://127.0.0.1:4340",
});
console.log(
  "Disposable UI fixture: sign-in code oac_waveform_ui_fixture; environment key " +
    fake.testKey,
);
for (const signal of ["SIGINT", "SIGTERM"])
  process.on(signal, () => {
    backend.close();
    server.close();
    process.exit(0);
  });
