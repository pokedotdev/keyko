# tierkey

Secrets in git, encrypted with [SOPS](https://github.com/getsops/sops) + [age](https://age-encryption.org), and shared with your team through the repository itself.

```bash
tierkey set dev DATABASE_URL     # prompted, hidden; encrypted into secrets/dev.env
tierkey run dev -- pnpm dev      # the values exist only in that process
```

- Secrets are dotenv files under `secrets/`: key names in plaintext, values encrypted. A diff shows which secret changed and who changed it.
- Nothing decrypted is written to disk, and no subcommand prints a secret value.
- Each file belongs to a **level** (`dev`, `preview`, `production`, ...), and each person opens only the levels they are given.

## Two ways to share secrets

A level opens in one of two ways. One project can mix them, level by level.

| | Per-person keys | Level keys |
| --- | --- | --- |
| How secrets reach your team | Through git alone. | Through a key server on [Tailscale](https://tailscale.com). |
| What each person has | Their own age key. | A tailnet login. |
| What you run | Only the `tierkey` binary: no server, no Tailscale. | A small key server. |
| Adding someone | They run `tierkey join`; you run the command it prints and push. | A tailnet admin adds them to a group. |
| A new machine | Copy the key over, or `join` again. | Log in to the tailnet. |
| Removing someone | `tierkey people remove`, then push. | Remove them from the group, then `tierkey rotate-level`. |

**Per-person keys** work like plain SOPS + age: each file is encrypted for the people given its level, and tierkey hands your own key to `sops`. They suit a small team, or any project that should not depend on a server. Every change of access is a commit.

**Level keys** keep one age key per level on the key server, which is reachable only on the tailnet, through `tailscale serve`. Tailscale tells it who is calling and which levels the policy grants them. The CLI fetches the keys it needs into memory for one command and never writes them to disk. Joining the team, or switching machines, takes nothing but logging in to the tailnet.

```
                 tailnet (WireGuard)               your server
 tierkey run ──────────────────────────▶ tailscale serve ──▶ tierkey-server
   │  GET /v1/keys/myapp?levels=dev     adds Tailscale-User-Login      127.0.0.1:8787
   │                                        and Tailscale-App-Capabilities  │ keys from env
   ▼                                                                         │
 sops decrypt secrets/dev.env  ◀── SOPS_AGE_KEY (in sops's environment only) ┘
```

In both modes, every file is also encrypted for the machine keys that build it (Cloudflare Pages, CI) and the maintainers' **break-glass** keys.

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
scripts/install-tierkey.sh 0.2.0 <sha256 of tierkey-linux-amd64> .cache/tierkey
```

While this repository is private, downloads need `TIERKEY_GITHUB_TOKEN` (a token that can read it).

## Quick start: per-person keys

Nothing to deploy. Start the project:

```bash
tierkey init                        # secrets/config.toml, with you holding every level
tierkey set dev DATABASE_URL        # prompted, hidden; creates secrets/dev.env
tierkey run dev -- pnpm dev
tierkey check && git add secrets && git commit -m "secrets: add DATABASE_URL" && git push
```

Add a teammate:

```bash
tierkey join                                  # they: creates their key, prints a command for you
tierkey people add bob age1... --levels dev   # you: run what it printed, then commit and push
```

They `git pull`, and `tierkey whoami` shows their levels. More in [People](docs/people.md).

## Quick start: level keys

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
   tierkey check && git add secrets && git commit -m "secrets: add DATABASE_URL"
   ```

Add a teammate: a tailnet admin adds their login to the project's group in the Tailscale policy. They log in to Tailscale and run `tierkey whoami`; nobody re-encrypts anything. More in [Key server](docs/key-server.md#onboarding-a-teammate).

To mix both modes in one project, see [Levels](docs/configuration.md#levels).

## Documentation

| | |
| --- | --- |
| [Configuration](docs/configuration.md) | `secrets/config.toml`: levels, people, machines, file rules, branches, Dokploy; targets. |
| [Commands](docs/commands.md) | Every subcommand, and which key each command uses. |
| [People](docs/people.md) | Per-person keys: joining, adding, removing, new machines, mixing with level keys. |
| [Key server](docs/key-server.md) | Setup (Tailscale policy, Docker Compose, Dokploy, systemd), onboarding over the tailnet, rotating a level. |
| [Machines](docs/machines.md) | CI and build platforms. |
| [Agents](docs/agents.md) | What Claude Code may do, how it gets `dev` access, and the agent skill. |
| [Migrating](docs/migrating.md) | From per-person SOPS + age, or from a tierkey 0.1 project that listed people under `[machines]`. |
| [Development](docs/development.md) | Building, testing and releasing tierkey. |

## Limits

- Removing someone means rotating the values they could open: with `people remove` for a person, `rotate-level` for key server access. Old versions stay readable in git history.
- With per-person keys, every change of access is a commit that re-encrypts files, made by someone who can open them.
- The key server records reads only as its fetch log: who fetched which levels and when, not which values they then opened.
- Machines keep their own age keys, outside the server.
- The break-glass key opens everything; keep it offline and with the maintainer.
- Anyone who can push a branch can use the secrets its build opens (for example `preview`).

## License

[MIT](LICENSE)
