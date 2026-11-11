# tierkey

Secrets in git, encrypted with SOPS + age, and shared with your team through the repository itself; a key server on Tailscale is optional. See the [main README](https://github.com/pokedotdev/tierkey#readme).

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
