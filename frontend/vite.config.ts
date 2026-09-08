import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
export default defineConfig({
  plugins: [solid()],
  build: { target: "es2022" },
  server: {
    host: "127.0.0.1",
    port: 4325,
    strictPort: true,
    proxy: {
      "/api/": "http://127.0.0.1:4326",
      "/health/": "http://127.0.0.1:4326",
      "/auth/": "http://127.0.0.1:4326",
    },
  },
});
