# tierkey

Secrets in git, encrypted with [SOPS](https://github.com/getsops/sops) + [age](https://age-encryption.org), opened by **level keys** that a small key server hands out over [Tailscale](https://tailscale.com). Joining the team, or switching machines, takes nothing but logging in to the tailnet.

- Each project keeps its secrets as dotenv files under `secrets/`: key names in plaintext, values encrypted. A diff shows which secret changed and who changed it.
- Access is by **level**, not by person. Each project has one age key per level (`dev`, `preview`, `production`, ...), and each file is encrypted for its level's key, the machine keys that build it (Cloudflare Pages, CI) and the maintainers' **break-glass** keys.
- The level private keys live only on the **key server**. It is reachable only on the tailnet, through `tailscale serve`, which tells it who is calling and which levels the Tailscale policy grants them. The CLI fetches the keys it needs into memory for one command and never writes them to disk.
- No subcommand prints a secret value.

```
                 tailnet (WireGuard)               your server
 tierkey run ──────────────────────────▶ tailscale serve ──▶ tierkey-server
   │  GET /v1/keys/myapp?levels=dev     adds Tailscale-User-Login      127.0.0.1:8787
   │                                        and Tailscale-App-Capabilities  │ keys from env
   ▼                                                                         │
 sops decrypt secrets/dev.env  ◀── SOPS_AGE_KEY (in sops's environment only) ┘
```

## Contents

- [Install](#install)
- [Project setup](#project-setup)
- [Commands](#commands)
- [Key server setup](#key-server-setup) (policy grants, `tailscale serve`, Docker Compose, Dokploy, systemd)
- [Onboarding a teammate](#onboarding-a-teammate)
- [Machines: CI and build platforms](#machines-ci-and-build-platforms)
- [Agents](#agents)
- [Removing someone and rotating a level](#removing-someone-and-rotating-a-level)
- [Migrating from per-person age keys](#migrating-from-per-person-age-keys)
- [Limits](#limits)
- [Development](#development)

## Install

tierkey calls the `sops` binary (3.9 or later). Where `sops` is not on `PATH` it downloads the pinned release (3.13.3) into `<project>/.cache/` and checks its SHA-256, so build machines need nothing else.

**Nix.** Add the flake as an input; its `tierkey` package has `sops` wrapped in.

```nix
{
  inputs.tierkey.url = "github:pokedotdev/tierkey";
  # In a dev shell:
  #   packages = [ inputs.tierkey.packages.${system}.default ];
}
```

**npm.** For JavaScript projects and build platforms that run `npm`/`pnpm install` (Cloudflare Pages, Netlify, Vercel):

```bash
pnpm add -D tierkey
pnpm exec tierkey run dev -- pnpm dev
```

The `tierkey` package pulls in the binary for the machine from `@tierkey/linux-x64`, `linux-arm64`, `darwin-x64` or `darwin-arm64` (optional dependencies, no install scripts), so the lockfile's integrity hashes pin it.

**Release binaries.** Each `v*` tag publishes `tierkey-{linux,darwin}-{amd64,arm64}` (static on Linux), `tierkey-server-linux-{amd64,arm64}` and `SHA256SUMS`. [`scripts/install.sh`](scripts/install.sh) downloads a pinned version and checks it against a pinned checksum; copy it into your project so the checksum lives in your git:

```bash
scripts/install-tierkey.sh 0.1.0 <sha256 of tierkey-linux-amd64> .cache/tierkey
```

While this repository is private, downloads need `TIERKEY_GITHUB_TOKEN` (a token that can read it).

**Cargo.** `cargo binstall tierkey` downloads the release binary for your platform; `cargo install tierkey` builds it from crates.io.

## Project setup

tierkey looks for `secrets/config.toml` in the current directory or above it.

```toml
# secrets/config.toml
project = "myapp"                          # the project's name on the key server
server = "https://tierkey.example.ts.net"     # the key server; TIERKEY_SERVER overrides it
auto = "{env}/pages.env"                       # what `run auto` opens (the default)

# Maintainers' break-glass public keys: they open every file. The private keys stay offline.
break_glass = ["age1..."]

# One key per level. Public keys here, private keys on the key server.
[levels]
dev = "age1..."
preview = "age1..."
production = "age1..."
ops = "age1..."

# Machines that hold their own private key (SOPS_AGE_KEY): CI, build platforms.
[machines]
ci = "age1..."
pages-preview = "age1..."

# Which level each file under secrets/ belongs to. The first matching glob wins; `*` matches
# slashes too, as in bash's [[ == ]].
[[files]]
path = "dev.env"
level = "dev"

[[files]]
path = "ops.env"          # deploy credentials, e.g. a hosting API key
level = "ops"

[[files]]
path = "production/*"
level = "production"
machines = ["ci"]

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
```

`secrets/.sops.yaml` is generated from this file (`tierkey rekey` writes it), so plain `sops` agrees with tierkey. Do not edit it by hand; `tierkey check` fails when it is stale.

### Optional: Dokploy

tierkey works with any hosting. If yours is [Dokploy](https://dokploy.com), it can also push files to applications (`sync`, `check --remote`) and save new level keys straight to the key server (`keygen --level`, `rotate-level`):

```toml
[dokploy]
url = "http://my-server.example.ts.net:3000"  # DOKPLOY_URL overrides it
credentials = "ops.env"                       # holds DOKPLOY_API_KEY (the default)

[dokploy.apps]                                # files to Dokploy application IDs
"production/api.env" = "<applicationId>"

[dokploy.key_server]                          # where tierkey-server runs
kind = "compose"                              # or "application"
id = "<composeId>"
```

Elsewhere, use `run` to hand the values to your platform's own CLI or deploy script, which read them from their environment (`tierkey run production/api -- ./deploy.sh`), and place level keys with `--out FILE`.

### Targets

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
| `check [--remote]` | Every file encrypted, for exactly its recipients; `.sops.yaml` current; no stray files; every environment mapped. Needs no key, so CI runs it. `--remote` also compares key names with Dokploy (if configured). |
| `recipients [target]` | The public keys that can open each file, labelled from the config. |
| `rekey` | Regenerates `.sops.yaml` and re-encrypts every file for its recipients. |
| `revoke <age-pubkey>` | After you remove a key from the config: rekeys, gives the files it could open new data keys, and lists the values to rotate. |
| `merge-fix <target>` | Re-signs a file after a hand-resolved git conflict. |
| `sync <target> [--dry-run] [--yes] [--deploy]` | Dokploy only: saves a file as its Dokploy application's environment, keeping build args, build secrets and the env-file setting. Shows changes by name only. |
| `pair list\|add\|remove <target> LIST [NAME [<target>:VAR ...]]` | Rotates one key at a time in a `name=key,...` list; `add` also sets the new key in each `<target>:VAR`. |
| `keygen [--out FILE]` | Creates a break-glass key (default `.age/key.txt` in the main checkout, shared by worktrees). |
| `keygen --level L [--out FILE] [--deploy] [--yes]` | Creates level `L`: its public key goes into the config, its private key to `--out FILE`, or to the key server's Dokploy service when `[dokploy.key_server]` is set. |
| `rotate-level L [--out FILE] [--deploy] [--yes]` | Replaces level `L`'s key: saves the new private key first, then re-encrypts every file of the level with new data keys, and lists the values the level could read. |
| `whoami` | Who the key server sees you as and which levels it gives you. |

### Which key is used

1. `SOPS_AGE_KEY` or `SOPS_AGE_KEY_FILE`, when set: machines (CI, build platforms).
2. The key server: tierkey asks for exactly the levels the command needs, once per run, and passes them to `sops` in its environment. They never reach the command `run` starts, nor the disk.
3. The break-glass key file: `TIERKEY_BREAK_GLASS_KEY_FILE`, else `.age/key.txt` next to the main checkout. Used when there is no server, it cannot be reached, or it does not give a level you need.

sops runs with an empty config directory, so it never picks up a stray `~/.config/sops/age/keys.txt`.

## Key server setup

One `tierkey-server` serves any number of projects. It takes level private keys from its environment, one variable per project and level:

```
TIERKEY_KEY__MYAPP__DEV=AGE-SECRET-KEY-1...
TIERKEY_KEY__MYAPP__PRODUCTION=AGE-SECRET-KEY-1NEW...,AGE-SECRET-KEY-1OLD...
```

Names are uppercased, dashes become underscores. A variable can hold several keys, newest first, so branches still encrypted for an older key keep opening during a rotation.

It trusts the identity headers that `tailscale serve` sets (and strips from incoming requests), so **nothing but `tailscale serve` may reach it**. It listens on `127.0.0.1:8787` and refuses a non-loopback address unless `TIERKEY_ALLOW_NON_LOOPBACK=1`.

| Variable | Default | |
| --- | --- | --- |
| `TIERKEY_LISTEN` | `127.0.0.1:8787` | |
| `TIERKEY_CAPABILITY` | `github.com/pokedotdev/tierkey/cap/tierkey` | The app capability it reads grants from. |
| `TIERKEY_ACCESS_FILE` | none | Fallback grants by login, for when app capabilities cannot be forwarded (below). |
| `TIERKEY_KEY__<PROJECT>__<LEVEL>` | | Level private keys. |

Endpoints: `GET /v1/keys/<project>?levels=a,b` (the granted subset, or 403), `GET /v1/access/<project>` (levels only), `GET /healthz`. Each request is logged to stdout with the caller's login, project and the levels given and denied; never a key.

### 1. Tailscale policy

App capabilities need Tailscale 1.92 or later on the server node. In the admin console, under **Access controls**, merge [`deploy/policy.hujson`](deploy/policy.hujson): groups for each project's people, `tag:tierkey` for the server, and grants such as

```jsonc
{
  "src": ["group:myapp-dev"],
  "dst": ["tag:tierkey"],
  "ip": ["tcp:443"],
  "app": {
    "github.com/pokedotdev/tierkey/cap/tierkey": [
      { "projects": { "myapp": ["dev", "preview"] } }
    ]
  }
}
```

Grants add up across groups. `"*"` stands for every project or every level the server holds (`{"projects": {"*": ["*"]}}`); use it sparingly. Tagged devices (servers, CI runners on the tailnet) carry no login but can be granted capabilities the same way; they show up as `tagged-node` in the log.

### 2. Auth key

Under **Settings > Keys**, generate an auth key: reusable off, ephemeral off, tags `tag:tierkey`. The sidecar setups below need it only for the node's first login; its state volume keeps it logged in, after which you can delete the key from the server's environment.

### 3. Run it

Pick whichever fits your hosting. In every case, check from a machine on the tailnet:

```bash
curl https://tierkey.<tailnet>.ts.net/healthz
tierkey whoami
```

If you use a different capability name, change it in `serve.json` (or `--accept-app-caps`), the policy and `TIERKEY_CAPABILITY`. If MagicDNS names the node `tierkey-1` (an old node kept the name), remove the old node or update `server` in each project.

#### Docker Compose with a Tailscale sidecar (recommended)

[`deploy/compose.yaml`](deploy/compose.yaml) runs two containers that share one network namespace:

- `tailscale` joins the tailnet as the node `tierkey` and runs `tailscale serve` from [`deploy/serve.json`](deploy/serve.json): HTTPS on 443 with a tailnet certificate, proxied to `127.0.0.1:8787`, forwarding the `github.com/pokedotdev/tierkey/cap/tierkey` capability.
- `tierkey` (built from the [`Dockerfile`](Dockerfile), a static binary on an empty image, running as nobody) listens on `127.0.0.1:8787` inside that namespace. Neither the host nor other containers can reach it.

On any host with Docker:

```bash
git clone https://github.com/pokedotdev/tierkey && cd tierkey/deploy
cat > .env <<'ENV'      # mode 600; it holds private keys
TS_AUTHKEY=tskey-auth-...
TIERKEY_KEY__MYAPP__DEV=AGE-SECRET-KEY-1...
ENV
chmod 600 .env
docker compose up -d --build
```

After changing `.env`, `docker compose up -d` restarts the server with the new keys. Any platform with sidecars or shared network namespaces works the same way, for example a Kubernetes pod with a Tailscale container and `TS_SERVE_CONFIG`.

#### On Dokploy

Create a **Compose** service from this repository with compose path `./deploy/compose.yaml`. In its **Environment**, set `TS_AUTHKEY` and the `TIERKEY_KEY__...` variables; Dokploy writes them to `.env` beside the compose file, which the `tierkey` container reads. Deploy. With `[dokploy.key_server]` in a project's config, `tierkey keygen --level` and `rotate-level` write the level keys there and `--deploy` restarts it.

#### As a binary on a host already on the tailnet

Install `tierkey-server-linux-<arch>` from a release, keep the keys in a root-only file, and run it bound to loopback, for example with systemd:

```ini
# /etc/systemd/system/tierkey.service
[Service]
ExecStart=/usr/local/bin/tierkey-server
EnvironmentFile=/etc/tierkey/keys.env
DynamicUser=yes
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

Then publish it on the tailnet (the host's Tailscale must be 1.92 or later), and set `server = "https://<host>.<tailnet>.ts.net:8443"` in projects:

```bash
tailscale serve --bg --https=8443 --accept-app-caps=github.com/pokedotdev/tierkey/cap/tierkey http://127.0.0.1:8787
```

In the policy grants, `dst` is then that host (or its tag) and `ip` is `tcp:8443`. Any process on that host could reach the port directly and claim any identity, which the sidecar rules out, so prefer it on shared hosts.

**Without app capabilities.** If you cannot forward capabilities, mount a TOML file and point `TIERKEY_ACCESS_FILE` at it; it maps `Tailscale-User-Login` values to grants. Grants from both sources add up.

```toml
[users."alice@github"]
myapp = ["dev", "preview"]
```

## Onboarding a teammate

1. A tailnet admin invites them and adds their login to the project's groups in the policy (for example `group:myapp-dev`).
2. They install Tailscale, log in with GitHub or Google, and install tierkey (the project's Nix dev shell may already have it).
3. In the project:

   ```bash
   tierkey whoami                    # login and the levels they get
   tierkey run dev -- pnpm install   # secrets only in that process
   ```

Nobody rekeys anything. A new machine is the same: log in to Tailscale, run a command.

## Machines: CI and build platforms

Machines cannot log in to the tailnet, so each keeps its own age key: create it with `tierkey keygen --out <file>`, add its public key under `[machines]` and to the `[[files]]` rules it may open, run `tierkey rekey`, and set the file's last line as the machine's `SOPS_AGE_KEY` secret. Then delete the file.

A GitHub Actions job:

```yaml
- run: tierkey run production/api -- ./deploy.sh
  env:
    SOPS_AGE_KEY: ${{ secrets.SOPS_AGE_KEY }}
```

A build platform that installs JavaScript dependencies first (Cloudflare Pages, Netlify, Vercel, ...), with `tierkey` in `devDependencies`:

```bash
pnpm exec tierkey run auto --optional -- npm run build
```

Without npm, use the pinned release:

```bash
scripts/install-tierkey.sh 0.1.0 <sha256> .cache/tierkey && .cache/tierkey run auto --optional -- npm run build
```

`auto` reads the branch from `CF_PAGES_BRANCH` or GitHub Actions; elsewhere name the file (`run preview/web`).

`check` needs no key, so CI can run `tierkey check` on every change to `secrets/`.

## Agents

When Claude Code runs tierkey (`CLAUDECODE=1`), it only ever asks the key server for the `dev` level, never falls back to the break-glass key, and refuses `edit`, `keygen --level` and `rotate-level`. Any other level fails before a request is made, with a message to leave the command to a maintainer. This is a guardrail, not a boundary: an agent with your shell could run the binary without the variable.

[`skills/tierkey/SKILL.md`](skills/tierkey/SKILL.md) is an agent skill that teaches this workflow: run with `dev` secrets, set random values, check before committing, and hand anything above `dev` to a person as ready-to-run commands. Copy it into a project's `.claude/skills/tierkey/` (or your agent's skills folder):

```bash
mkdir -p .claude/skills/tierkey && cp path/to/tierkey/skills/tierkey/SKILL.md .claude/skills/tierkey/
```

## Removing someone and rotating a level

Removing someone from a group stops new fetches at once, but they may have kept the level keys they fetched. So:

```bash
tierkey rotate-level preview --out new-preview-key.txt   # any hosting
tierkey rotate-level preview --deploy                    # with [dokploy.key_server]
```

It generates a new key pair and saves the private key first, so a failure later loses nothing: with `--out FILE` to a new file for you to put on the key server (in front of the old key, comma-separated), or, with `[dokploy.key_server]`, straight into the key server's Dokploy environment, redeployed with `--deploy`. Next it re-encrypts every file of the level with new data keys (sops `rotate`, so old copies of the data key are useless) and the new public key, updates `secrets/config.toml` and `.sops.yaml`, and lists the values the level could open. Rotate those values at their source, because the old ciphertexts stay readable in git history. Commit `secrets/`; once no branch still needs the old key, remove it from the `TIERKEY_KEY__...` variable.

To drop a machine or break-glass key: remove it from the config, then `tierkey revoke <age-pubkey>`.

## Migrating from per-person age keys

For a project that uses SOPS with one age key per person (for example a script with `.age/key.txt` files and a hand-written `secrets/.sops.yaml`):

1. **Write `secrets/config.toml`.** Copy the path rules from `.sops.yaml` into `[[files]]` (in the same order) and the branch map into `[[branches]]`. Put the maintainer's existing public key in `break_glass`: their `.age/key.txt` becomes the break-glass key and keeps working (it is the default break-glass path). Move machine keys to `[machines]`. List each level with an empty key, `dev = ""`, until it is created; `check` reports them.
2. **Deploy the key server** with no level keys yet, and the policy grants (see [Key server setup](#key-server-setup)).
3. **Create the levels**, with the maintainer's key: `tierkey keygen --level dev --out dev-key.txt` writes each level's public key into the config and its private key to a file; put its last line in the server's `TIERKEY_KEY__<PROJECT>__DEV`, restart the server and delete the file. With `[dokploy.key_server]`, `--yes --deploy` saves it there instead. Then `tierkey whoami` shows the levels you get.
4. **Re-encrypt.** `tierkey rekey` replaces `.sops.yaml` with the generated one and re-encrypts every file for its level, machines and break-glass key. Personal keys are no longer recipients.
5. **New data keys.** `rekey` only rewraps each file's data key, so a personal key that opened an old version in git history can still open the data key and, with it, values set later. For every personal key other than the break-glass one, run `tierkey revoke <age-pubkey>`: it gives the files it could open new data keys and lists the values to rotate at their source if that person should lose access.
6. **Switch the tooling.** Replace the old script with a shim (`exec tierkey "$@"`), update the CI check to `tierkey check`, build commands, and the docs. Delete personal `.age/` keys other than the maintainer's break-glass key.
7. `tierkey check`, commit.

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
nix build .#tierkey      # the wrapped CLI
docker build -t tierkey-server .
```

The integration tests run the binary against throwaway git repositories with throwaway age keys. Without `sops` they are skipped, unless `TIERKEY_REQUIRE_SOPS=1` (set in the dev shell and CI).

### Releases

Bump `version` in the root `Cargo.toml` (and the `tierkey-core` dependency version beside it), commit, then tag and push:

```bash
git tag v0.2.0 && git push origin v0.2.0
```

[`release.yml`](.github/workflows/release.yml) checks the tag matches the version, builds the binaries, publishes the GitHub release with `SHA256SUMS`, and publishes `tierkey-core` and `tierkey` to crates.io (`tierkey-server` is not published), and the `tierkey` and `@tierkey/<platform>` packages to npm ([`npm/build.mjs`](npm/build.mjs) assembles them from the release binaries).

crates.io accepts the very first publish of a crate only with an API token, so set one for the first release:

```bash
gh secret set CARGO_REGISTRY_TOKEN --repo pokedotdev/tierkey   # paste a token scoped to publish-new and publish-update
```

Afterwards, in each crate's **Settings > Trusted Publishing** on crates.io, add GitHub with owner `pokedotdev`, repository `tierkey` and workflow `release.yml`, then delete the secret (`gh secret delete CARGO_REGISTRY_TOKEN --repo pokedotdev/tierkey`). Later releases get a short-lived token through OIDC.

npm works the same way. For the first release, set an npm token that can publish `tierkey` and the `@tierkey` scope:

```bash
gh secret set NPM_TOKEN --repo pokedotdev/tierkey
```

Afterwards, on npmjs.com, open each of the five packages' **Settings > Trusted Publisher**, choose GitHub Actions with owner `pokedotdev`, repository `tierkey` and workflow `release.yml`, then delete the secret. While the repository is private, packages are published without provenance.

## License

[MIT](LICENSE)
