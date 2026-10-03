import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri の開発サーバー規約: ポート 1420 固定(tauri.conf.json の devUrl と一致させる)
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    // Rust のビルド成果物(src-tauri/target)を監視しない。cargo がリンク中の
    // agent_deck.exe を vite が watch して EBUSY で落ちるため(Tauri 標準テンプレートと同じ)。
    watch: { ignored: ["**/src-tauri/**"] },
  },
});
