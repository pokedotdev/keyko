#!/usr/bin/env node
// Runs the keyko binary from the @keyko/<platform> package npm installed for this machine.
"use strict";

const { spawn } = require("node:child_process");
const fs = require("node:fs");

const PLATFORMS = {
  "linux x64": "@keyko/linux-x64",
  "linux arm64": "@keyko/linux-arm64",
  "darwin x64": "@keyko/darwin-x64",
  "darwin arm64": "@keyko/darwin-arm64",
};

function fail(message) {
  process.stderr.write(`keyko: ${message}\n`);
  process.exit(1);
}

const pkg = PLATFORMS[`${process.platform} ${process.arch}`];
if (!pkg) {
  fail(`no prebuilt binary for ${process.platform} ${process.arch}; install it with cargo install keyko`);
}

let bin;
try {
  bin = require.resolve(`${pkg}/bin/keyko`);
} catch {
  fail(`${pkg} is not installed; reinstall without --no-optional or --omit=optional`);
}

// Some package managers drop the executable bit when they unpack.
try {
  fs.accessSync(bin, fs.constants.X_OK);
} catch {
  try {
    fs.chmodSync(bin, 0o755);
  } catch {}
}

const child = spawn(bin, process.argv.slice(2), { stdio: "inherit" });
// Pass termination on, so a cancelled build stops keyko and the command it runs.
const signals = ["SIGINT", "SIGTERM", "SIGHUP"];
for (const signal of signals) {
  process.on(signal, () => child.kill(signal));
}
child.on("error", (error) => fail(`cannot run ${bin}: ${error.message}`));
child.on("exit", (code, signal) => {
  if (signal) {
    for (const s of signals) process.removeAllListeners(s);
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 1);
  }
});
