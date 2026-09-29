// Reuse the browser's synthetic data for native instrumentation; never shipped in the app.
import { createRequire } from "node:module";
import { mkdirSync, writeFileSync, realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";
const require = createRequire(realpathSync(new URL("../desktop/node_modules/vite/package.json", import.meta.url)));
const { build } = require("esbuild");
const result = await build({ entryPoints: [fileURLToPath(new URL("../desktop/src/preview.ts", import.meta.url))], bundle: true, platform: "node", format: "esm", write: false });
globalThis.location = { search: "?mobile=1" };
const fixture = await import(`data:text/javascript;base64,${Buffer.from(result.outputFiles[0].text).toString("base64")}`);
const state = await fixture.run("get_state", {});
for (const group of state.snapshot.groups) group.recent_accounts ??= [];
const config = await fixture.run("api_request", { method: "GET", path: "/config" });
const directory = new URL("../desktop/src-tauri/gen/android/app/src/androidTest/assets/", import.meta.url);
mkdirSync(directory, { recursive: true });
for (const [name, value] of [["snapshot", state.snapshot], ["config", config]]) writeFileSync(new URL(`${name}.json`, directory), JSON.stringify(value));
console.log("Generated synthetic Android test fixtures");
