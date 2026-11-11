# Machines: CI and build platforms

Machines cannot log in to the tailnet or keep a person's key, so each keeps its own age key:

1. Create it: `tierkey keygen --out <file>`.
2. Add its public key under `[machines]`, and its name to the `machines = [...]` of the `[[files]]` rules it may open.
3. Run `tierkey rekey`.
4. Set the file's last line as the machine's `SOPS_AGE_KEY` secret, then delete the file.

## GitHub Actions

```yaml
- run: tierkey run production/api -- ./deploy.sh
  env:
    SOPS_AGE_KEY: ${{ secrets.SOPS_AGE_KEY }}
```

## Build platforms

A build platform that installs JavaScript dependencies first (Cloudflare Pages, Netlify, Vercel, ...), with `tierkey` in `devDependencies`:

```bash
pnpm exec tierkey run auto --optional -- npm run build
```

Without npm, use the pinned release:

```bash
scripts/install-tierkey.sh 0.1.1 <sha256> .cache/tierkey && .cache/tierkey run auto --optional -- npm run build
```

`auto` reads the branch from `CF_PAGES_BRANCH` or GitHub Actions; elsewhere, name the file (`run preview/web`). With `--optional`, an environment that has no file yet builds with the platform's own variables.

## Checking in CI

`check` needs no key, so CI can run `tierkey check` on every change to `secrets/`.
