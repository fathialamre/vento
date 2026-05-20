import path from "node:path";
import { defineConfig } from "vitest/config";

export default defineConfig({
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  test: {
    // Pure-logic unit tests live next to the source they cover.
    // No JSX / DOM yet; reach for jsdom once we test React components.
    environment: "node",
    include: ["src/**/*.{test,spec}.ts"],
  },
});
