import { readdirSync, statSync } from "node:fs";
import { join } from "node:path";

const bundlePath = "web/dist/app.js";
const sourcePaths = [
  "package.json",
  "bun.lock",
  "tsconfig.json",
  ...walk("web/src"),
];

let bundleStat;
try {
  bundleStat = statSync(bundlePath);
} catch {
  fail(`${bundlePath} is missing. Run bun run build:web.`);
}

const staleSource = sourcePaths
  .map((path) => [path, statSync(path)])
  .find(([, stat]) => stat.mtimeMs > bundleStat.mtimeMs);

if (staleSource) {
  fail(`${bundlePath} is older than ${staleSource[0]}. Run bun run build:web.`);
}

function walk(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    return entry.isDirectory() ? walk(path) : [path];
  });
}

function fail(message) {
  console.error(`web bundle stale: ${message}`);
  process.exit(1);
}
