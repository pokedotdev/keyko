# tierkey

Encrypted secrets in git with [SOPS](https://github.com/getsops/sops) + [age](https://age-encryption.org), shared with your team.

```bash
tierkey set dev DATABASE_URL     # prompted, hidden; encrypted into secrets/dev.env
tierkey run dev -- pnpm dev      # the values exist only in that process
```

- Secrets are dotenv files under `secrets/`, committed to git: key names in plaintext, values encrypted. A diff shows which secret changed and who changed it.
- Each file belongs to a level (`dev`, `preview`, `production`, ...), and you decide who can open each level.
- Nothing decrypted is written to disk, and no subcommand prints a secret value.

## Who can open the secrets

People open secrets with a key, which they get in one of two ways. Use either, or both in one project:

- **Personal keys.** Each person has their own age key, listed in `secrets/config.toml` with the levels they can open. Nothing else to run; adding or removing someone is a command and a commit.
- **A key server.** Each level has one key, kept on a small server on [Tailscale](https://tailscale.com). Access is managed in the Tailscale policy; joining or switching machines is logging in to the tailnet.

Machine keys (CI, Cloudflare Pages) and maintainers' break-glass keys can open files too.

## Install

tierkey calls the `sops` binary (3.9 or later). Where `sops` is not on `PATH`, tierkey downloads the pinned release (3.13.3) into `<project>/.cache/` and checks its SHA-256, so build machines need nothing else.

**npm**, for JavaScript projects and for build platforms that run `npm`/`pnpm install` (Cloudflare Pages, Netlify, Vercel):

```bash
pnpm add -D tierkey
pnpm exec tierkey run dev -- pnpm dev
```

The `tierkey` package pulls in the binary for the machine from `@tierkey/linux-x64`, `linux-arm64`, `darwin-x64` or `darwin-arm64`. They are optional dependencies with no install scripts, so the lockfile's integrity hashes pin the binary.

**Nix.** Add the flake as an input; its `tierkey` package has `sops` wrapped in.

```nix
{
  inputs.tierkey.url = "github:pokedotdev/tierkey";
  # In a dev shell:
  #   packages = [ inputs.tierkey.packages.${system}.default ];
}
```

**Cargo.** `cargo binstall tierkey` downloads the release binary for your platform; `cargo install tierkey` builds it from crates.io.

**Release binaries.** Each `v*` tag publishes `tierkey-{linux,darwin}-{amd64,arm64}` (static on Linux), `tierkey-server-linux-{amd64,arm64}` and `SHA256SUMS`. [`scripts/install.sh`](scripts/install.sh) downloads a pinned version and checks it against a pinned checksum. Copy it into your project, so the checksum lives in your git:

```bash
scripts/install-tierkey.sh 0.3.0 <sha256 of tierkey-linux-amd64> .cache/tierkey
```

While this repository is private, downloads need `TIERKEY_GITHUB_TOKEN` (a token that can read it).

## Quick start: personal keys

Start the project:

```bash
tierkey init                        # secrets/config.toml, with you holding every level
tierkey set dev DATABASE_URL        # prompted, hidden; creates secrets/dev.env
tierkey run dev -- pnpm dev
tierkey check && git add secrets && git commit -m "secrets: add DATABASE_URL" && git push
```

Add a teammate:

```bash
tierkey people invite bob     # you: adds bob to dev, commits and pushes, prints a one-time code
tierkey join                  # they, in their clone: paste the code; their own key replaces it
```

Or, with nothing secret sent: they run `tierkey join` and send you the `tierkey people add bob age1...` line it prints; you run it, commit and push.

They `git pull`, and `tierkey whoami` shows their levels. Later:

```bash
tierkey people add bob --levels production    # more levels
tierkey people remove bob                     # no access; lists the values to rotate
```

More in [People](docs/people.md).

## Quick start: key server

1. [Set up the key server](docs/key-server.md#setup) and its Tailscale policy grants.
2. Write `secrets/config.toml`, listing each level with an empty key until it is created:

   ```toml
   project = "myapp"
   server = "https://tierkey.<tailnet>.ts.net"

   [levels]
   dev = ""
   production = ""

   [[files]]
   path = "dev.env"
   level = "dev"

   [[files]]
   path = "*"
   level = "production"
   ```

3. Create each level. Its public key goes into the config, and its private key to a file:

   ```bash
   tierkey keygen --level dev --out dev-key.txt
   tierkey keygen --level production --out production-key.txt
   ```

4. Put each file's last line on the key server as `TIERKEY_KEY__MYAPP__DEV` and `TIERKEY_KEY__MYAPP__PRODUCTION`, restart it, and delete the files. With Dokploy, `--yes --deploy` saves the keys there instead (see [Configuration](docs/configuration.md#optional-dokploy)).
5. Use it:

   ```bash
   tierkey whoami                      # your tailnet login and the levels the server gives you
   tierkey set dev DATABASE_URL        # prompted, hidden; creates secrets/dev.env
   tierkey run dev -- pnpm dev         # the level key is fetched into memory for this run only
   tierkey check && git add secrets && git commit -m "secrets: add DATABASE_URL" && git push
   ```

Add a teammate: a tailnet admin adds their login to the project's group in the Tailscale policy. They log in to Tailscale and run `tierkey whoami`; nobody re-encrypts anything. More in [Key server](docs/key-server.md).

## Using both

- **Add personal keys to a project with a key server:** `tierkey people add NAME KEY --levels L`. Those people then open their levels without the server.
- **Add a key server to a project with personal keys:** set up the server, add `server = "..."` to the config, and for each level run `tierkey keygen --level L --out FILE`, put the key on the server, then `tierkey rekey`.

## Documentation

| | |
| --- | --- |
| [Configuration](docs/configuration.md) | `secrets/config.toml`: levels, people, machines, file rules, branches, Dokploy; targets. |
| [Commands](docs/commands.md) | Every subcommand, and which key each command uses. |
| [People](docs/people.md) | Personal keys: joining, adding, removing, new machines, combining with a key server. |
| [Key server](docs/key-server.md) | Setup (Tailscale policy, Docker Compose, Dokploy, systemd), onboarding over the tailnet, rotating a level. |
| [Machines](docs/machines.md) | CI and build platforms. |
| [Agents](docs/agents.md) | What Claude Code may do, how it gets `dev` access, and the agent skill. |
| [Migrating](docs/migrating.md) | An existing SOPS + age project, or a tierkey 0.1 project that listed people under `[machines]`. |
| [Development](docs/development.md) | Building, testing and releasing tierkey. |

## Limits

- Removing someone means rotating the values they could open: `people remove` lists them, and `rotate-level` does it for key server access. Old versions stay readable in git history.
- With personal keys, every change of access is a commit that re-encrypts files, made by someone who can open them.
- An invite code opens the levels it gives (only `dev` unless you pass `--any-level`) for whoever has it, until it is used.
- The key server records reads only as its fetch log: who fetched which levels and when, not which values they then opened.
- Machines keep their own age keys, outside the server.
- The break-glass key opens everything; keep it offline and with the maintainer.
- Anyone who can push a branch can use the secrets its build opens (for example `preview`).

## License

[MIT](LICENSE)
