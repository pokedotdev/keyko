# tierkey

Secrets in git, encrypted with SOPS + age, opened by each person's own age key, by level keys that a small key server hands out over Tailscale, or both. See the [main README](https://github.com/pokedotdev/tierkey#readme).

This package installs the prebuilt `tierkey` binary for Linux or macOS (x64, arm64) through one of the `@tierkey/<platform>` packages, without install scripts. The lockfile's integrity hashes pin it.

```bash
pnpm add -D tierkey
pnpm exec tierkey run dev -- pnpm dev
```

A build command, for example on Cloudflare Pages (which runs `pnpm install` first):

```bash
pnpm exec tierkey run auto --optional -- npm run build
```

tierkey calls `sops`; where it is not installed, tierkey downloads a pinned release and checks its SHA-256.
