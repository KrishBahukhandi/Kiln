import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  // Served from a project page on GitHub Pages, so assets need a base path.
  // Override with KILN_SITE_BASE when hosting at a domain root.
  base: process.env.KILN_SITE_BASE ?? "/",
});
