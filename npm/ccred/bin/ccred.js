#!/usr/bin/env node
"use strict";

// Thin launcher for the prebuilt binary.
//
// The binary is NOT downloaded at install time. Each platform's binary ships
// as its own npm package, listed in optionalDependencies with `os`/`cpu`
// fields, so npm installs exactly the right one and skips the rest. This is
// the esbuild/biome pattern, and for a tool that holds OAuth tokens the
// reasons matter:
//
//   * it works under `npm ci --ignore-scripts`, increasingly the default in
//     security-conscious setups, where a postinstall downloader fails open;
//   * it installs offline and from a cache, with the binary pinned by the
//     lockfile;
//   * npm's provenance attestation then covers the actual binary rather than
//     a fetcher shim that pulls an unverified URL afterwards.

const { spawnSync } = require("node:child_process");

function libcSuffix() {
  if (process.platform !== "linux") return "";
  // glibcVersionRuntime is absent on musl. The `libc` package.json field is
  // honoured by newer npm/pnpm/yarn but not by all of them, so this runtime
  // check stays as the fallback -- do not remove it.
  const report = process.report && process.report.getReport();
  const runtime = report && report.header && report.header.glibcVersionRuntime;
  return runtime ? "" : "-musl";
}

const pkg = `@ccred/${process.platform}-${process.arch}${libcSuffix()}`;
const exe = process.platform === "win32" ? "ccred.exe" : "ccred";

let binary;
try {
  binary = require.resolve(`${pkg}/${exe}`);
} catch {
  console.error(
    `ccred: no prebuilt binary for ${process.platform}-${process.arch}.\n` +
      `Install another way instead:\n` +
      `  cargo install ccred\n` +
      `  https://github.com/Zigecek/ccred#install`
  );
  process.exit(1);
}

// stdio: "inherit" hands the real terminal through, so interactive prompts and
// Ctrl-C behave exactly as they do for the native binary.
const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });

if (result.error) {
  console.error(`ccred: could not run ${binary}: ${result.error.message}`);
  process.exit(1);
}
// A child killed by a signal reports null; treat that as a failure rather than
// success, so a scheduler never mistakes a kill for a clean run.
process.exit(result.status === null ? 1 : result.status);
