#!/usr/bin/env node
// Checks that every place Lumen's version is declared agrees, and — when a tag is passed — that
// the release tag matches it. Run by CI and by the release workflow before anything is built.
//
//   node scripts/check-version.mjs            # consistency only
//   node scripts/check-version.mjs v0.1.12    # also require the tag to match
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (path) => readFileSync(join(root, path), "utf8");
const json = (path) => JSON.parse(read(path));

const pkg = json("package.json");
const lock = json("package-lock.json");
const tauri = json("src-tauri/tauri.conf.json");
const cargoToml = read("src-tauri/Cargo.toml");
const cargoLock = read("Cargo.lock");
const pkgbuild = read("packaging/aur/PKGBUILD");
const metainfo = read("packaging/com.quinton.lumen.metainfo.xml");

const match = (text, pattern) => text.match(pattern)?.[1];

const declared = [
  ["package.json", pkg.version],
  ["package-lock.json", lock.version],
  ['package-lock.json packages[""]', lock.packages?.[""]?.version],
  ["src-tauri/tauri.conf.json", tauri.version],
  [
    "src-tauri/Cargo.toml",
    match(cargoToml, /^\[package\][\s\S]*?^version\s*=\s*"([^"]+)"/m),
  ],
  [
    "Cargo.lock (lumen)",
    match(cargoLock, /\[\[package\]\]\nname = "lumen"\nversion = "([^"]+)"/),
  ],
  ["packaging/aur/PKGBUILD", match(pkgbuild, /^pkgver=(.+)$/m)],
  [
    "packaging/com.quinton.lumen.metainfo.xml",
    match(metainfo, /<release version="([^"]+)"/),
  ],
];

let failed = false;
for (const [where, version] of declared) {
  if (!version) {
    console.error(`missing version in ${where}`);
    failed = true;
  }
}

const expected = pkg.version;
for (const [where, version] of declared) {
  if (version && version !== expected) {
    console.error(`${where} is ${version}, expected ${expected}`);
    failed = true;
  }
}

const tag = process.argv[2]?.trim();
if (tag) {
  const tagVersion = tag.replace(/^v/, "");
  if (tagVersion !== expected) {
    console.error(`release tag ${tag} does not match the app version ${expected}`);
    failed = true;
  }
}

if (failed) {
  process.exit(1);
}
console.log(
  `Lumen version ${expected} matches every declaration${tag ? ` and tag ${tag}` : ""}`,
);
