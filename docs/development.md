# Development

```bash
nix develop               # cargo, sops, age
cargo test                # unit and integration tests (fake key server, fake Dokploy)
nix build .#tierkey       # the wrapped CLI
docker build -t tierkey-server .
```

The integration tests run the binary against throwaway git repositories with throwaway age keys. Without `sops` on `PATH` they are skipped, unless `TIERKEY_REQUIRE_SOPS=1` (set in the dev shell and CI) makes that a failure.

## Releases

Bump `version` in the root `Cargo.toml` (and the `tierkey-core` dependency version beside it), commit, then tag and push:

```bash
git tag v0.2.0 && git push origin v0.2.0
```

[`release.yml`](../.github/workflows/release.yml) then:

- checks that the tag matches the version;
- builds the binaries and publishes the GitHub release with `SHA256SUMS`;
- publishes `tierkey-core` and `tierkey` to crates.io (`tierkey-server` is not published);
- publishes the `tierkey` and `@tierkey/<platform>` packages to npm ([`npm/build.mjs`](../npm/build.mjs) assembles them from the release binaries).

### crates.io

crates.io accepts the very first publish of a crate only with an API token, so set one for the first release:

```bash
gh secret set CARGO_REGISTRY_TOKEN --repo pokedotdev/tierkey   # paste a token scoped to publish-new and publish-update
```

Afterwards, in each crate's **Settings > Trusted Publishing** on crates.io, add GitHub with owner `pokedotdev`, repository `tierkey` and workflow `release.yml`, then delete the secret (`gh secret delete CARGO_REGISTRY_TOKEN --repo pokedotdev/tierkey`). Later releases get a short-lived token through OIDC.

### npm

npm publishes through Trusted Publishing only, with no token in CI. A trusted publisher is configured on a package's npmjs.com settings, so the five packages must exist first. Once, from a maintainer's own npm login:

```bash
npm login
npm/bootstrap.sh    # publishes placeholder 0.0.0 versions of tierkey and @tierkey/<platform>
```

Then, for each of the five packages, open **Settings > Trusted Publisher** on npmjs.com, choose GitHub Actions with owner `pokedotdev`, repository `tierkey` and workflow `release.yml`, and allow `npm publish`.

- Push the release tag within two days: npm expires a trusted publisher that has not published yet.
- After that release, deprecate the placeholders (`bootstrap.sh` prints the command).
- Provenance is added automatically once the repository is public.
