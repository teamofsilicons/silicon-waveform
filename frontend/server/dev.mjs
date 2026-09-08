import { createServer } from "vite";
import { startServer } from "./node.mjs";
const gateway = startServer({
  port: 4326,
  origin: process.env.WAVEFORM_FRONTEND_ORIGIN || "http://localhost:4325",
});
const vite = await createServer();
await vite.listen();
vite.printUrls();
for (const signal of ["SIGTERM", "SIGINT"])
  process.on(signal, async () => {
    await vite.close();
    gateway.close();
    process.exit(0);
  });
