import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
export default defineConfig({
  plugins: [react()],
  build: { target: ["chrome98", "safari15.4"] },
  server: { port: 1420, strictPort: true },
  clearScreen: false,
});
