# tierkey

Secrets in git, encrypted with [SOPS](https://github.com/getsops/sops) + [age](https://age-encryption.org).

Each project keeps its secrets as dotenv files under `secrets/`: key names in plaintext, values encrypted, so a diff shows which secret changed and who changed it. Each file belongs to a **level** (`dev`, `preview`, `production`, ...), and a level opens in either of two ways, which one project can mix:

| | People | Level keys |
| --- | --- | --- |
| Who holds a key | Each person has their own age key. | One age key per level, shared. |
| How files are encrypted | For the people given the file's level, as with plain SOPS + age. | For the level's key. |
| What it needs | Only the `tierkey` binary: no server, no Tailscale. | A small key server on [Tailscale](https://tailscale.com). |
| Joining, or a new machine | `tierkey join`, then a maintainer runs the command it prints. | Logging in to the tailnet. |

Every file is also encrypted for the machine keys that build it (Cloudflare Pages, CI) and the maintainers' **break-glass** keys. No subcommand prints a secret value.

## How the key server works

The level private keys live only on the key server. It is reachable only on the tailnet, through `tailscale serve`, which tells it who is calling and which levels the Tailscale policy grants them. The CLI fetches the keys it needs into memory for one command and never writes them to disk.

```
                 tailnet (WireGuard)               your server
 tierkey run ──────────────────────────▶ tailscale serve ──▶ tierkey-server
   │  GET /v1/keys/myapp?levels=dev     adds Tailscale-User-Login      127.0.0.1:8787
   │                                        and Tailscale-App-Capabilities  │ keys from env
   ▼                                                                         │
 sops decrypt secrets/dev.env  ◀── SOPS_AGE_KEY (in sops's environment only) ┘
```

With per-person keys there is nothing in the middle: tierkey hands your own key to `sops`.

## Install

tierkey calls the `sops` binary (3.9 or later). Where `sops` is not on `PATH`, it downloads the pinned release (3.13.3) into `<project>/.cache/` and checks its SHA-256, so build machines need nothing else.

**Nix.** Add the flake as an input. Its `tierkey` package has `sops` wrapped in.

```nix
{
  inputs.tierkey.url = "github:pokedotdev/tierkey";
  # In a dev shell:
  #   packages = [ inputs.tierkey.packages.${system}.default ];
}
```

**npm.** For JavaScript projects, and build platforms that run `npm`/`pnpm install` (Cloudflare Pages, Netlify, Vercel):

```bash
pnpm add -D tierkey
pnpm exec tierkey run dev -- pnpm dev
```

The `tierkey` package pulls in the binary for the machine from `@tierkey/linux-x64`, `linux-arm64`, `darwin-x64` or `darwin-arm64`. They are optional dependencies with no install scripts, so the lockfile's integrity hashes pin the binary.

**Release binaries.** Each `v*` tag publishes `tierkey-{linux,darwin}-{amd64,arm64}` (static on Linux), `tierkey-server-linux-{amd64,arm64}` and `SHA256SUMS`. [`scripts/install.sh`](scripts/install.sh) downloads a pinned version and checks it against a pinned checksum. Copy it into your project, so the checksum lives in your git:

```bash
scripts/install-tierkey.sh 0.2.0 <sha256 of tierkey-linux-amd64> .cache/tierkey
```

While this repository is private, downloads need `TIERKEY_GITHUB_TOKEN` (a token that can read it).

**Cargo.** `cargo binstall tierkey` downloads the release binary for your platform; `cargo install tierkey` builds it from crates.io.

## Quick start

Pick the mode that fits; a project can mix both later (see [Levels](docs/configuration.md#levels)).

### Per-person keys (no server)

Everyone uses their own key, with nothing to deploy:

```bash
tierkey init                        # secrets/config.toml, with you holding every level
tierkey set dev DATABASE_URL        # prompted, hidden; creates secrets/dev.env
tierkey run dev -- pnpm dev         # the values exist only in that process
tierkey check && git add secrets && git commit -m "secrets: add DATABASE_URL"
```

Someone joins:

```bash
tierkey join                                  # they: creates their key, prints a command for you
tierkey people add bob age1... --levels dev   # you: run what it printed, then commit and push
```

They `git pull`, and `tierkey whoami` shows their levels. More in [People](docs/people.md).

### Level keys (key server on Tailscale)

First [set up the key server](docs/key-server.md#setup) and its Tailscale policy grants. Then write `secrets/config.toml`, listing each level with an empty key until it is created:

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

Create each level. Its public key goes into the config and its private key to a file:

```bash
tierkey keygen --level dev --out dev-key.txt
tierkey keygen --level production --out production-key.txt
```

Put each file's last line on the key server as `TIERKEY_KEY__MYAPP__DEV` and `TIERKEY_KEY__MYAPP__PRODUCTION`, restart it, and delete the files. With Dokploy, `--yes --deploy` saves the keys there instead (see [Configuration](docs/configuration.md#optional-dokploy)). Then:

```bash
tierkey whoami                      # your tailnet login and the levels the server gives you
tierkey set dev DATABASE_URL        # prompted, hidden; creates secrets/dev.env
tierkey run dev -- pnpm dev         # the level key is fetched into memory for this run only
tierkey check && git add secrets && git commit -m "secrets: add DATABASE_URL"
```

Someone joins: a tailnet admin adds their login to the project's group in the Tailscale policy. They log in to Tailscale and run `tierkey whoami`; nobody re-encrypts anything. More in [Key server](docs/key-server.md#onboarding-a-teammate).

## Documentation

| | |
| --- | --- |
| [Configuration](docs/configuration.md) | `secrets/config.toml`: levels, people, machines, file rules, branches, Dokploy; targets. |
| [Commands](docs/commands.md) | Every subcommand, and which key each command uses. |
| [People](docs/people.md) | One key per person: joining, adding, removing, new machines, mixing with level keys. |
| [Key server](docs/key-server.md) | Setup (Tailscale policy, Docker Compose, Dokploy, systemd), onboarding over the tailnet, rotating a level. |
| [Machines](docs/machines.md) | CI and build platforms. |
| [Agents](docs/agents.md) | What Claude Code may do, how it gets `dev` access, and the agent skill. |
| [Migrating](docs/migrating.md) | From per-person SOPS + age, or from a tierkey 0.1 project that listed people under `[machines]`. |
| [Development](docs/development.md) | Building, testing and releasing tierkey. |

## Limits

- The key server records reads only as its fetch log: who fetched which levels and when, not which values they then opened.
- Removing someone means rotating the values they could open: with `people remove` for a person, `rotate-level` for key server access.
- With per-person keys, every change of access is a commit that re-encrypts files, made by someone who can open them.
- Machines keep their own age keys, outside the server.
- The break-glass key opens everything; keep it offline and with the maintainer.
- Anyone who can push a branch can use the secrets its build opens (for example `preview`).

## License

[MIT](LICENSE)
