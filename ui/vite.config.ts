import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Dev server port. Override with the ACCOUNTING_DEV_PORT env var if 5280 is
// taken; keep tauri.conf.json's devUrl in sync when changing the default.
const DEV_PORT = Number(process.env.ACCOUNTING_DEV_PORT) || 5280;

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: DEV_PORT, strictPort: true },
  envPrefix: ["VITE_", "TAURI_"],
});
