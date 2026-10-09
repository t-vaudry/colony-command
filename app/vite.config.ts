import { defineConfig } from "vite";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

// In dev, the page reads the daemon's port and token through this route.
// The Tauri build reads the same file through a command instead.
export default defineConfig({
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  plugins: [
    {
      name: "colony-daemon-info",
      configureServer(server) {
        server.middlewares.use("/__colony/daemon.json", (_req, res) => {
          try {
            res.setHeader("content-type", "application/json");
            res.end(readFileSync(join(process.env.COLONY_HOME ?? join(homedir(), ".colony"), "daemon.json")));
          } catch {
            res.statusCode = 503;
            res.end(JSON.stringify({ error: "colonyd is not running (no ~/.colony/daemon.json)" }));
          }
        });
      },
    },
  ],
});
