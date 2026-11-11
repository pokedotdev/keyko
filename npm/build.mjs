#!/usr/bin/env node
// Assembles the npm packages for one release: `tierkey` (the launcher) and one
// `@tierkey/<platform>` package per release binary. Run from the repository root:
//
//   node npm/build.mjs --version 0.1.0 --dist dist --out npm-dist
//
// dist holds the release binaries (tierkey-linux-amd64, ...). Platforms whose binary is missing
// are skipped, unless --all is given, so a local build can test just its own platform.
import { chmodSync, copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";

const { values } = parseArgs({
  options: {
    version: { type: "string" },
    dist: { type: "string" },
    out: { type: "string" },
    all: { type: "boolean", default: false },
  },
});
const { version, dist, out, all } = values;
if (!version || !dist || !out || !/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version)) {
  console.error("usage: build.mjs --version X.Y.Z --dist DIR --out DIR [--all]");
  process.exit(2);
}

// npm's os/cpu names, and the release asset each package carries.
const PLATFORMS = [
  { name: "linux-x64", os: "linux", cpu: "x64", asset: "tierkey-linux-amd64" },
  { name: "linux-arm64", os: "linux", cpu: "arm64", asset: "tierkey-linux-arm64" },
  { name: "darwin-x64", os: "darwin", cpu: "x64", asset: "tierkey-darwin-amd64" },
  { name: "darwin-arm64", os: "darwin", cpu: "arm64", asset: "tierkey-darwin-arm64" },
];

const here = new URL(".", import.meta.url).pathname;
const template = JSON.parse(readFileSync(join(here, "tierkey/package.json"), "utf8"));
const repository = (directory) => ({ ...template.repository, directory });

rmSync(out, { recursive: true, force: true });
const built = [];
for (const p of PLATFORMS) {
  const binary = join(dist, p.asset);
  if (!existsSync(binary)) {
    if (all) {
      console.error(`missing ${binary}`);
      process.exit(1);
    }
    continue;
  }
  const dir = join(out, p.name);
  mkdirSync(join(dir, "bin"), { recursive: true });
  copyFileSync(binary, join(dir, "bin/tierkey"));
  chmodSync(join(dir, "bin/tierkey"), 0o755);
  const pkg = {
    name: `@tierkey/${p.name}`,
    version,
    description: `The tierkey binary for ${p.os} ${p.cpu}; install the tierkey package instead`,
    homepage: template.homepage,
    license: template.license,
    repository: repository("npm"),
    os: [p.os],
    cpu: [p.cpu],
    files: ["bin"],
    preferUnplugged: true,
  };
  writeFileSync(join(dir, "package.json"), `${JSON.stringify(pkg, null, 2)}\n`);
  writeFileSync(join(dir, "README.md"), `# ${pkg.name}\n\n${pkg.description}: https://www.npmjs.com/package/tierkey\n`);
  copyFileSync(join(here, "../LICENSE"), join(dir, "LICENSE"));
  built.push(pkg.name);
}

const main = join(out, "tierkey");
cpSync(join(here, "tierkey"), main, { recursive: true });
copyFileSync(join(here, "../LICENSE"), join(main, "LICENSE"));
const pkg = {
  ...template,
  version,
  // Every platform, even ones not built here, so the published launcher covers them all.
  optionalDependencies: Object.fromEntries(PLATFORMS.map((p) => [`@tierkey/${p.name}`, version])),
};
writeFileSync(join(main, "package.json"), `${JSON.stringify(pkg, null, 2)}\n`);

// Publish order: platforms first, so the launcher never points at a version that is not there.
writeFileSync(join(out, "order.txt"), `${[...PLATFORMS.filter((p) => built.includes(`@tierkey/${p.name}`)).map((p) => p.name), "tierkey"].join("\n")}\n`);
console.log(`built ${[...built, "tierkey"].join(", ")} ${version} in ${out}`);
