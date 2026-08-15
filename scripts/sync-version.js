#!/usr/bin/env node
import { readFileSync, writeFileSync } from "fs";
import { resolve, dirname } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = resolve(__dirname, "..");

const rootPkg = JSON.parse(readFileSync(resolve(root, "package.json"), "utf8"));
const version = rootPkg.version;

const packages = [
  "packages/clients/taladb",
  "packages/bindings/web",
  "packages/bindings/node",
  "packages/bindings/react-native",
  "packages/clients/react",
  "packages/integrations/cloudflare",
];

for (const pkg of packages) {
  const pkgPath = resolve(root, pkg, "package.json");
  const pkgJson = JSON.parse(readFileSync(pkgPath, "utf8"));
  pkgJson.version = version;

  // Sync taladb / @taladb/* dependency references, but leave workspace:* untouched
  for (const depField of [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
  ]) {
    if (!pkgJson[depField]) continue;
    for (const dep of Object.keys(pkgJson[depField])) {
      if (
        (dep === "taladb" || dep.startsWith("@taladb/")) &&
        pkgJson[depField][dep] !== "workspace:*"
      ) {
        pkgJson[depField][dep] = `^${version}`;
      }
    }
  }

  writeFileSync(pkgPath, JSON.stringify(pkgJson, null, 2) + "\n");
  console.log(`✓ ${pkg} → ${version}`);
}

// Sync Cargo.toml workspace version
const cargoPath = resolve(root, "Cargo.toml");
let cargo = readFileSync(cargoPath, "utf8");
cargo = cargo.replace(/^(version\s*=\s*)"[^"]*"/m, `$1"${version}"`);
writeFileSync(cargoPath, cargo);
console.log(`✓ Cargo.toml → ${version}`);

// Sync VitePress nav version badge
const vpConfigPath = resolve(root, "docs/.vitepress/config.mts");
let vpConfig = readFileSync(vpConfigPath, "utf8");
vpConfig = vpConfig.replace(/(text:\s*"v)\d+\.\d+\.\d+(")/, `$1${version}$2`);
writeFileSync(vpConfigPath, vpConfig);
console.log(`✓ docs/.vitepress/config.mts → v${version}`);
