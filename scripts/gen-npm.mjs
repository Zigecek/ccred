#!/usr/bin/env node
// Lay out the npm packages from the release artifacts.
//
//   node scripts/gen-npm.mjs --from dist --out target/npm --version 0.1.0
//
// Produces one package per platform plus the root wrapper, with every version
// stamped consistently. Publish the platform packages FIRST: the root names
// exact versions in optionalDependencies, so publishing it first leaves a
// window where it resolves to versions that do not exist.

import {
  mkdirSync,
  copyFileSync,
  writeFileSync,
  readFileSync,
  existsSync,
  cpSync,
} from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

/** Rust target triple -> npm platform identity. */
const TARGETS = [
  { triple: "x86_64-unknown-linux-gnu", pkg: "linux-x64", os: "linux", cpu: "x64", libc: "glibc" },
  { triple: "x86_64-unknown-linux-musl", pkg: "linux-x64-musl", os: "linux", cpu: "x64", libc: "musl" },
  { triple: "aarch64-unknown-linux-gnu", pkg: "linux-arm64", os: "linux", cpu: "arm64", libc: "glibc" },
  { triple: "aarch64-unknown-linux-musl", pkg: "linux-arm64-musl", os: "linux", cpu: "arm64", libc: "musl" },
  { triple: "x86_64-apple-darwin", pkg: "darwin-x64", os: "darwin", cpu: "x64" },
  { triple: "aarch64-apple-darwin", pkg: "darwin-arm64", os: "darwin", cpu: "arm64" },
  { triple: "x86_64-pc-windows-msvc", pkg: "win32-x64", os: "win32", cpu: "x64" },
];

function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`);
  if (i !== -1 && process.argv[i + 1]) return process.argv[i + 1];
  if (fallback !== undefined) return fallback;
  throw new Error(`missing --${name}`);
}

const from = arg("from");
const out = arg("out", "target/npm");
const version = arg("version");

mkdirSync(join(out, "platforms"), { recursive: true });

const optional = {};
let built = 0;

for (const t of TARGETS) {
  const exe = t.os === "win32" ? "ccred.exe" : "ccred";
  // cargo-dist unpacks each archive into a directory named after the triple.
  const src = join(from, `ccred-${t.triple}`, exe);
  if (!existsSync(src)) {
    console.warn(`skipping ${t.pkg}: no artifact at ${src}`);
    continue;
  }

  const dir = join(out, "platforms", t.pkg);
  mkdirSync(dir, { recursive: true });
  copyFileSync(src, join(dir, exe));

  const manifest = {
    name: `@ccred/${t.pkg}`,
    version,
    description: `ccred binary for ${t.os} ${t.cpu}.`,
    license: "MIT OR Apache-2.0",
    repository: { type: "git", url: "git+https://github.com/Zigecek/ccred.git" },
    os: [t.os],
    cpu: [t.cpu],
    files: [exe],
    // Yarn PnP would otherwise keep the binary inside a zip, where it cannot
    // be executed.
    preferUnplugged: true,
  };
  if (t.libc) manifest.libc = [t.libc];

  writeFileSync(join(dir, "package.json"), JSON.stringify(manifest, null, 2) + "\n");
  optional[`@ccred/${t.pkg}`] = version;
  built += 1;
}

if (built === 0) throw new Error(`no artifacts found under ${from}`);

// Root wrapper: the committed template with versions stamped in.
const rootDir = join(out, "ccred");
cpSync(join(root, "npm", "ccred"), rootDir, { recursive: true });

const rootManifest = JSON.parse(readFileSync(join(rootDir, "package.json"), "utf8"));
rootManifest.version = version;
rootManifest.optionalDependencies = optional;
writeFileSync(join(rootDir, "package.json"), JSON.stringify(rootManifest, null, 2) + "\n");

console.log(`generated ${built} platform packages plus the root wrapper in ${out}`);
console.log("publish order: platforms first, root last");
