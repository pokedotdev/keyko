# keyko

Encrypted secrets in git with SOPS + age, shared with your team. See the [main README](https://github.com/pokedotdev/keyko#readme).

This package installs the prebuilt `keyko` binary for Linux or macOS (x64, arm64) through one of the `@keyko/<platform>` packages, without install scripts. The lockfile's integrity hashes pin it.

```bash
pnpm add -D keyko
pnpm exec keyko run dev -- pnpm dev
```

A build command, for example on Cloudflare Pages (which runs `pnpm install` first):

```bash
pnpm exec keyko run auto --optional -- npm run build
```

keyko calls `sops`; where it is not installed, keyko downloads a pinned release and checks its SHA-256.
