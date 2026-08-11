import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri の開発サーバー規約: ポート 1420 固定(tauri.conf.json の devUrl と一致させる)
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
});
