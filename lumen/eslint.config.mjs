import js from "@eslint/js";
import globals from "globals";
import reactHooks from "eslint-plugin-react-hooks";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist", "node_modules", "target", "src-tauri", "crates", "sync-server", "public"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    languageOptions: { globals: { ...globals.browser } },
    plugins: { "react-hooks": reactHooks },
    rules: {
      // The two rules that catch real bugs in hooks: calling them conditionally, and stale dependencies.
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "error",
      // `const { token, ...safe } = config` is how a field is left out on purpose.
      "@typescript-eslint/no-unused-vars": ["error", { ignoreRestSiblings: true }],
    },
  },
  {
    // The plain-Node checks run directly with `node --experimental-strip-types`.
    files: ["src/**/*.test.ts"],
    languageOptions: { globals: { ...globals.node } },
  },
);
