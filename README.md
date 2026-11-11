# litekeys

Secrets in git, encrypted with [SOPS](https://github.com/getsops/sops) + [age](https://age-encryption.org), opened by **level keys** that a small key server hands out over [Tailscale](https://tailscale.com). Joining the team, or switching machines, takes nothing but logging in to the tailnet.

- Each project keeps its secrets as dotenv files under `secrets/`: key names in plaintext, values encrypted. A diff shows which secret changed and who changed it.
- Access is by **level**, not by person. Each project has one age key per level (`dev`, `preview`, `production`, ...), and each file is encrypted for its level's key, the machine keys that build it (Cloudflare Pages, CI) and the maintainers' **break-glass** keys.
- The level private keys live only on the **key server**. It is reachable only on the tailnet, through `tailscale serve`, which tells it who is calling and which levels the Tailscale policy grants them. The CLI fetches the keys it needs into memory for one command and never writes them to disk.
- No subcommand prints a secret value.

```
                 tailnet (WireGuard)               VPS / Dokploy
 litekeys run ──────────────────────────▶ tailscale serve ──▶ litekeys-server
   │  GET /v1/keys/myapp?levels=dev     adds Tailscale-User-Login      127.0.0.1:8787
   │                                        and Tailscale-App-Capabilities  │ keys from env
   ▼                                                                         │
 sops decrypt secrets/dev.env  ◀── SOPS_AGE_KEY (in sops's environment only) ┘
```

## Contents

- [Install](#install)
- [Project setup](#project-setup)
- [Commands](#commands)
- [Key server setup](#key-server-setup) (Dokploy, `tailscale serve`, policy grants)
- [Onboarding a teammate](#onboarding-a-teammate)
- [Machines: Cloudflare Pages and CI](#machines-cloudflare-pages-and-ci)
- [Agents](#agents)
- [Removing someone and rotating a level](#removing-someone-and-rotating-a-level)
- [Migrating from per-person age keys](#migrating-from-per-person-age-keys)
- [Limits](#limits)
- [Development](#development)

## Install

litekeys calls the `sops` binary (3.9 or later). Where `sops` is not on `PATH` it downloads the pinned release (3.13.3) into `<project>/.cache/` and checks its SHA-256, so build machines need nothing else.

**Nix.** Add the flake as an input; its `litekeys` package has `sops` wrapped in.

```nix
{
  inputs.litekeys.url = "github:pokedotdev/litekeys";
  # In a dev shell:
  #   packages = [ inputs.litekeys.packages.${system}.default ];
}
```

**Release binaries.** Each `v*` tag publishes `litekeys-{linux,darwin}-{amd64,arm64}` (static on Linux), `litekeys-server-linux-{amd64,arm64}` and `SHA256SUMS`. [`scripts/install.sh`](scripts/install.sh) downloads a pinned version and checks it against a pinned checksum; copy it into your project so the checksum lives in your git:

```bash
scripts/install-litekeys.sh 0.1.0 <sha256 of litekeys-linux-amd64> .cache/litekeys
```

While this repository is private, downloads need `LITEKEYS_GITHUB_TOKEN` (a token that can read it).

**From source.** `cargo install --git https://github.com/pokedotdev/litekeys litekeys`.

## Project setup

litekeys looks for `secrets/config.toml` in the current directory or above it.

```toml
# secrets/config.toml
project = "myapp"                          # the project's name on the key server
server = "https://litekeys.example.ts.net"     # the key server; LITEKEYS_SERVER overrides it
auto = "{env}/pages.env"                       # what `run auto` opens (the default)

# Maintainers' break-glass public keys: they open every file. The private keys stay offline.
break_glass = ["age1..."]

# One key per level. Public keys here, private keys on the key server.
[levels]
dev = "age1..."
preview = "age1..."
production = "age1..."
ops = "age1..."

# Machines that hold their own private key (SOPS_AGE_KEY): Cloudflare Pages, CI.
[machines]
pages-preview = "age1..."

# Which level each file under secrets/ belongs to. The first matching glob wins; `*` matches
# slashes too, as in bash's [[ == ]].
[[files]]
path = "dev.env"
level = "dev"

[[files]]
path = "ops.env"          # the Dokploy API key
level = "ops"

[[files]]
path = "production/*"
level = "production"

[[files]]
path = "*"                # staging, preview and branch environments
level = "preview"
machines = ["pages-preview"]

# Branch globs to environments; the first match wins. Every directory under secrets/ must be
# one of these environments.
[[branches]]
branch = "main"
env = "production"

[[branches]]
branch = "staging/*"
env = "staging"

[[branches]]
branch = "*"
env = "preview"

# Optional: Dokploy, for `sync`, `check --remote` and saving level keys to the key server.
[dokploy]
url = "http://my-vps.example.ts.net:3000"     # DOKPLOY_URL overrides it
credentials = "ops.env"                       # holds DOKPLOY_API_KEY (the default)

[dokploy.apps]                                # files to Dokploy application IDs
"production/api.env" = "<applicationId>"

[dokploy.key_server]                          # where litekeys-server runs
kind = "compose"                              # or "application"
id = "<composeId>"
```

`secrets/.sops.yaml` is generated from this file (`litekeys rekey` writes it), so plain `sops` agrees with litekeys. Do not edit it by hand; `litekeys check` fails when it is stale.

A target is `dev`, `<env>/<file>` (`production/api`), `auto` (the `auto` file of the current branch's environment: `CF_PAGES_BRANCH`, then the GitHub Actions branch, then git), or a path to a `.env` file under `secrets/`.

## Commands

| Command | What it does |
| --- | --- |
| `run <target> [--optional] -- <cmd...>` | Runs `cmd` with the file's values in its environment; they exist nowhere else. With `--optional`, a missing file runs `cmd` without it. |
| `env [branch]` | The environment a branch deploys to. |
| `names <target>` | Key names, without decrypting. |
| `set <target> NAME [--random N]` | Sets a value: prompted (hidden), from stdin, or `N` random bytes as hex. Creates the file. |
| `unset <target> NAME` | Removes a key. |
| `edit <target>` | The decrypted file in `$EDITOR` (people only). |
| `check [--remote]` | Every file encrypted, for exactly its recipients; `.sops.yaml` current; no stray files; every environment mapped. Needs no key, so CI runs it. `--remote` also compares key names with Dokploy. |
| `recipients [target]` | The public keys that can open each file, labelled from the config. |
| `rekey` | Regenerates `.sops.yaml` and re-encrypts every file for its recipients. |
| `revoke <age-pubkey>` | After you remove a key from the config: rekeys, gives the files it could open new data keys, and lists the values to rotate. |
| `merge-fix <target>` | Re-signs a file after a hand-resolved git conflict. |
| `sync <target> [--dry-run] [--yes] [--deploy]` | Saves a file as its Dokploy application's environment, keeping build args, build secrets and the env-file setting. Shows changes by name only. |
| `pair list\|add\|remove <target> LIST [NAME [<target>:VAR ...]]` | Rotates one key at a time in a `name=key,...` list; `add` also sets the new key in each `<target>:VAR`. |
| `keygen [--out FILE]` | Creates a break-glass key (default `.age/key.txt` in the main checkout, shared by worktrees). |
| `keygen --level L [--out FILE] [--deploy] [--yes]` | Creates level `L`: its public key goes into the config, its private key to the key server's Dokploy service (or `--out`). |
| `rotate-level L [--out FILE] [--deploy] [--yes]` | Replaces level `L`'s key: saves the new private key first, then re-encrypts every file of the level with new data keys, and lists the values the level could read. |
| `whoami` | Who the key server sees you as and which levels it gives you. |

### Which key is used

1. `SOPS_AGE_KEY` or `SOPS_AGE_KEY_FILE`, when set: machines (Cloudflare Pages, CI).
2. The key server: litekeys asks for exactly the levels the command needs, once per run, and passes them to `sops` in its environment. They never reach the command `run` starts, nor the disk.
3. The break-glass key file: `LITEKEYS_BREAK_GLASS_KEY_FILE`, else `.age/key.txt` next to the main checkout. Used when there is no server, it cannot be reached, or it does not give a level you need.

sops runs with an empty config directory, so it never picks up a stray `~/.config/sops/age/keys.txt`.

## Key server setup

One `litekeys-server` serves any number of projects. It takes level private keys from its environment, one variable per project and level:

```
LITEKEYS_KEY__MYAPP__DEV=AGE-SECRET-KEY-1...
LITEKEYS_KEY__MYAPP__PRODUCTION=AGE-SECRET-KEY-1NEW...,AGE-SECRET-KEY-1OLD...
```

Names are uppercased, dashes become underscores. A variable can hold several keys, newest first, so branches still encrypted for an older key keep opening during a rotation.

It trusts the identity headers that `tailscale serve` sets (and strips from incoming requests), so **nothing but `tailscale serve` may reach it**. It listens on `127.0.0.1:8787` and refuses a non-loopback address unless `LITEKEYS_ALLOW_NON_LOOPBACK=1`.

| Variable | Default | |
| --- | --- | --- |
| `LITEKEYS_LISTEN` | `127.0.0.1:8787` | |
| `LITEKEYS_CAPABILITY` | `github.com/pokedotdev/litekeys/cap/litekeys` | The app capability it reads grants from. |
| `LITEKEYS_ACCESS_FILE` | none | Fallback grants by login, for when app capabilities cannot be forwarded (below). |
| `LITEKEYS_KEY__<PROJECT>__<LEVEL>` | | Level private keys. |

Endpoints: `GET /v1/keys/<project>?levels=a,b` (the granted subset, or 403), `GET /v1/access/<project>` (levels only), `GET /healthz`. Each request is logged to stdout with the caller's login, project and the levels given and denied; never a key.

### 1. Tailscale policy

App capabilities need Tailscale 1.92 or later on the server node. In the admin console, under **Access controls**, merge [`deploy/policy.hujson`](deploy/policy.hujson): groups for each project's people, `tag:litekeys` for the server, and grants such as

```jsonc
{
  "src": ["group:myapp-dev"],
  "dst": ["tag:litekeys"],
  "ip": ["tcp:443"],
  "app": {
    "github.com/pokedotdev/litekeys/cap/litekeys": [
      { "projects": { "myapp": ["dev", "preview"] } }
    ]
  }
}
```

Grants add up across groups. `"*"` stands for every project or every level the server holds (`{"projects": {"*": ["*"]}}`); use it sparingly. Tagged devices (servers, CI runners on the tailnet) carry no login but can be granted capabilities the same way; they show up as `tagged-node` in the log.

### 2. Auth key

Under **Settings > Keys**, generate an auth key: reusable off, ephemeral off, tags `tag:litekeys`. It is only needed for the node's first login; the node's state volume keeps it logged in, after which you can delete the key from Dokploy.

### 3. Dokploy

Create a **Compose** service from this repository with compose path `./deploy/compose.yaml`. It runs two containers that share one network namespace:

- `tailscale` joins the tailnet as the node `litekeys` and runs `tailscale serve` from [`deploy/serve.json`](deploy/serve.json): HTTPS on 443 with a tailnet certificate, proxied to `127.0.0.1:8787`, forwarding the `github.com/pokedotdev/litekeys/cap/litekeys` capability.
- `litekeys` (built from the [`Dockerfile`](Dockerfile), a static binary on an empty image, running as nobody) listens on `127.0.0.1:8787` inside that namespace. Neither the host nor other containers can reach it.

In the service's **Environment**, set `TS_AUTHKEY` and the `LITEKEYS_KEY__...` variables (or let `litekeys keygen --level` and `rotate-level` write them). Dokploy writes them to `.env` beside the compose file, which the `litekeys` container reads. Deploy, then check from a machine on the tailnet:

```bash
curl https://litekeys.<tailnet>.ts.net/healthz
litekeys whoami
```

If you use a different capability name, change it in `serve.json`, the policy and `LITEKEYS_CAPABILITY`. If MagicDNS names the node `litekeys-1` (an old node kept the name), remove the old node or update `server` in each project.

**Without the sidecar.** On a host that is already on the tailnet, run the server bound to the host's loopback (for example compose with `network_mode: host`) and publish it with

```bash
tailscale serve --bg --https=8443 --accept-app-caps=github.com/pokedotdev/litekeys/cap/litekeys http://127.0.0.1:8787
```

Any process on that host could then reach the port directly and claim any identity, which the sidecar rules out.

**Without app capabilities.** If you cannot forward capabilities, mount a TOML file and point `LITEKEYS_ACCESS_FILE` at it; it maps `Tailscale-User-Login` values to grants. Grants from both sources add up.

```toml
[users."alice@github"]
myapp = ["dev", "preview"]
```

## Onboarding a teammate

1. A tailnet admin invites them and adds their login to the project's groups in the policy (for example `group:myapp-dev`).
2. They install Tailscale, log in with GitHub or Google, and install litekeys (the project's Nix dev shell may already have it).
3. In the project:

   ```bash
   litekeys whoami                    # login and the levels they get
   litekeys run dev -- pnpm install   # secrets only in that process
   ```

Nobody rekeys anything. A new machine is the same: log in to Tailscale, run a command.

## Machines: Cloudflare Pages and CI

Machines cannot log in to the tailnet, so each keeps its own age key: create it with `litekeys keygen --out <file>`, add its public key under `[machines]` and to the `[[files]]` rules it may open, run `litekeys rekey`, and set the file's last line as the machine's `SOPS_AGE_KEY` secret. Then delete the file.

A Cloudflare Pages build command:

```bash
scripts/install-litekeys.sh 0.1.0 <sha256> .cache/litekeys && .cache/litekeys run auto --optional -- npm run build
```

`check` needs no key, so CI can run `litekeys check` on every change to `secrets/`.

## Agents

When Claude Code runs litekeys (`CLAUDECODE=1`), it only ever asks the key server for the `dev` level, never falls back to the break-glass key, and refuses `edit`, `keygen --level` and `rotate-level`. Any other level fails before a request is made, with a message to leave the command to a maintainer. This is a guardrail, not a boundary: an agent with your shell could run the binary without the variable.

## Removing someone and rotating a level

Removing someone from a group stops new fetches at once, but they may have kept the level keys they fetched. So:

```bash
litekeys rotate-level preview --deploy
```

It generates a new key pair and saves the private key to the key server's Dokploy service, in front of the old one, then deploys it. Next it re-encrypts every file of the level with new data keys (sops `rotate`, so old copies of the data key are useless) and the new public key, updates `secrets/config.toml` and `.sops.yaml`, and lists the values the level could open. Rotate those values at their source, because the old ciphertexts stay readable in git history. Commit `secrets/`; once no branch still needs the old key, remove it from the `LITEKEYS_KEY__...` variable. Use `--out FILE` instead of Dokploy to place the key yourself.

To drop a machine or break-glass key: remove it from the config, then `litekeys revoke <age-pubkey>`.

## Migrating from per-person age keys

For a project that uses SOPS with one age key per person (for example a script with `.age/key.txt` files and a hand-written `secrets/.sops.yaml`):

1. **Write `secrets/config.toml`.** Copy the path rules from `.sops.yaml` into `[[files]]` (in the same order) and the branch map into `[[branches]]`. Put the maintainer's existing public key in `break_glass`: their `.age/key.txt` becomes the break-glass key and keeps working (it is the default break-glass path). Move machine keys to `[machines]`. List each level with an empty key, `dev = ""`, until it is created; `check` reports them.
2. **Deploy the key server** with no level keys yet, and the policy grants (see [Key server setup](#key-server-setup)).
3. **Create the levels**, with the maintainer's key: `litekeys keygen --level dev --yes --deploy` saves each private key to the key server's Dokploy service (`[dokploy.key_server]`) and writes its public key into the config. `--out FILE` instead writes the key for you to set by hand. Then `litekeys whoami` shows the levels you get.
4. **Re-encrypt.** `litekeys rekey` replaces `.sops.yaml` with the generated one and re-encrypts every file for its level, machines and break-glass key. Personal keys are no longer recipients.
5. **New data keys.** `rekey` only rewraps each file's data key, so a personal key that opened an old version in git history can still open the data key and, with it, values set later. For every personal key other than the break-glass one, run `litekeys revoke <age-pubkey>`: it gives the files it could open new data keys and lists the values to rotate at their source if that person should lose access.
6. **Switch the tooling.** Replace the old script with a shim (`exec litekeys "$@"`), update the CI check to `litekeys check`, the Pages build command, and the docs. Delete personal `.age/` keys other than the maintainer's break-glass key.
7. `litekeys check`, commit.

## Limits

- Reads are only recorded as the server's fetch log: who fetched which levels and when, not which values they then opened.
- Removing someone means rotating their levels and the values those levels opened.
- Machines keep their own age keys, outside the server.
- The break-glass key opens everything; keep it offline and with the maintainer.
- Anyone who can push a branch can use the secrets its build opens (for example `preview`).

## Development

```bash
nix develop               # cargo, sops, age
cargo test                # unit and integration tests (fake key server, fake Dokploy)
nix build .#litekeys      # the wrapped CLI
docker build -t litekeys-server .
```

The integration tests run the binary against throwaway git repositories with throwaway age keys. Without `sops` they are skipped, unless `LITEKEYS_REQUIRE_SOPS=1` (set in the dev shell and CI).

Releases: push a `v*` tag; [`release.yml`](.github/workflows/release.yml) builds the binaries and publishes them with `SHA256SUMS`.
