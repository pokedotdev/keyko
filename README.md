# tierkey

Secrets in git, encrypted with [SOPS](https://github.com/getsops/sops) + [age](https://age-encryption.org), and shared with your team through the repository itself. No server, no extra service: just git and the `tierkey` binary.

```bash
tierkey set dev DATABASE_URL     # prompted, hidden; encrypted into secrets/dev.env
tierkey run dev -- pnpm dev      # the values exist only in that process
```

## How it works

- **Secrets** are dotenv files under `secrets/`: key names in plaintext, values encrypted. A diff shows which secret changed and who changed it. Nothing decrypted is written to disk, and no subcommand prints a secret value.
- **Levels.** Each file belongs to a level (`dev`, `preview`, `production`, ...). Everyone given a level reads the same secrets: one file, one value, for all of them.
- **People.** Each person has their own age key, and `secrets/config.toml` says which levels each person gets. Every file of a level is encrypted for the people given it, as with plain SOPS + age, so they open it with their own key, offline.
- **Changing access** is a command and a commit: `tierkey people add` or `remove` re-encrypts the files involved, you push, and the others `git pull`.

```toml
# secrets/config.toml
project = "myapp"

[people]
alice = { key = "age1...", levels = ["dev", "production"] }
bob = { key = "age1...", levels = ["dev"] }

[[files]]
path = "dev.env"            # alice and bob share these secrets
level = "dev"

[[files]]
path = "production/*"       # only alice opens these
level = "production"
```

Every file is also encrypted for the machine keys that build it (Cloudflare Pages, CI) and the maintainers' **break-glass** keys.

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

## Quick start

Start the project:

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

They `git pull`, and `tierkey whoami` shows their levels. Later:

```bash
tierkey people add bob --levels production    # more levels
tierkey people remove bob                     # no access; lists the values to rotate
```

More in [People](docs/people.md).

## Optional: a key server on Tailscale

If your team is on [Tailscale](https://tailscale.com), a small key server can also hand out a level's key by tailnet login. A level then has one shared age key, kept on the server, and joining or switching machines takes nothing but logging in to the tailnet: no commit, no re-encrypting.

It is an addition, not a replacement. You choose per level: a level can be opened by its people, by the key server, or both, and someone listed under `[people]` keeps using their own key for their levels, without asking the server.

The server is reachable only on the tailnet, through `tailscale serve`, which tells it who is calling and which levels the Tailscale policy grants them. The CLI fetches the keys it needs into memory for one command and never writes them to disk.

```
                 tailnet (WireGuard)               your server
 tierkey run ──────────────────────────▶ tailscale serve ──▶ tierkey-server
   │  GET /v1/keys/myapp?levels=dev     adds Tailscale-User-Login      127.0.0.1:8787
   │                                        and Tailscale-App-Capabilities  │ keys from env
   ▼                                                                         │
 sops decrypt secrets/dev.env  ◀── SOPS_AGE_KEY (in sops's environment only) ┘
```

To set it up:

1. [Set up the key server](docs/key-server.md#setup) and its Tailscale policy grants, and add `server = "https://tierkey.<tailnet>.ts.net"` to `secrets/config.toml`.
2. Give each level a shared key. Its public key goes into the config, and its private key to a file:

   ```bash
   tierkey keygen --level dev --out dev-key.txt
   tierkey keygen --level production --out production-key.txt
   ```

3. Put each file's last line on the key server as `TIERKEY_KEY__MYAPP__DEV` and `TIERKEY_KEY__MYAPP__PRODUCTION`, restart it, and delete the files. With Dokploy, `--yes --deploy` saves the keys there instead (see [Configuration](docs/configuration.md#optional-dokploy)).
4. Run `tierkey rekey`, so the files are encrypted for the new keys too, then `tierkey check`, and commit.

`tierkey whoami` then shows your tailnet login and the levels the server gives you. To add a teammate, a tailnet admin adds their login to the project's group in the Tailscale policy; nobody re-encrypts anything. A project can also use the key server alone, with no `[people]` at all: see [A project with the key server alone](docs/key-server.md#a-project-with-the-key-server-alone).

## Documentation

| | |
| --- | --- |
| [Configuration](docs/configuration.md) | `secrets/config.toml`: levels, people, machines, file rules, branches, Dokploy; targets. |
| [Commands](docs/commands.md) | Every subcommand, and which key each command uses. |
| [People](docs/people.md) | Joining, adding, removing, new machines, mixing with the key server. |
| [Key server](docs/key-server.md) | Setup (Tailscale policy, Docker Compose, Dokploy, systemd), onboarding over the tailnet, rotating a level. |
| [Machines](docs/machines.md) | CI and build platforms. |
| [Agents](docs/agents.md) | What Claude Code may do, how it gets `dev` access, and the agent skill. |
| [Migrating](docs/migrating.md) | From per-person SOPS + age, or from a tierkey 0.1 project that listed people under `[machines]`. |
| [Development](docs/development.md) | Building, testing and releasing tierkey. |

## Limits

- Removing someone means rotating the values they could open: `people remove` lists them, and `rotate-level` does it for key server access. Old versions stay readable in git history.
- Every change of access for people is a commit that re-encrypts files, made by someone who can open them.
- The key server records reads only as its fetch log: who fetched which levels and when, not which values they then opened.
- Machines keep their own age keys, outside the server.
- The break-glass key opens everything; keep it offline and with the maintainer.
- Anyone who can push a branch can use the secrets its build opens (for example `preview`).

## License

[MIT](LICENSE)
