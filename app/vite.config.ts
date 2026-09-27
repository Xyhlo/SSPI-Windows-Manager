import { readFileSync } from "node:fs"
import path from "node:path"
import tailwindcss from "@tailwindcss/vite"
import react from "@vitejs/plugin-react"
import { defineConfig } from "vite"

const pkg = JSON.parse(readFileSync(path.resolve(__dirname, "package.json"), "utf8")) as { version: string }

export default defineConfig({
  plugins: [react(), tailwindcss()],
  define: { __APP_VERSION__: JSON.stringify(pkg.version) },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  build: { outDir: "../../Build-Output/Windows Manager/frontend", emptyOutDir: true },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
})
