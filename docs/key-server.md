# The key server

One `keyko-server` serves level keys for any number of projects, over Tailscale. Projects that only use [personal keys](people.md) do not need it.

- [How it works](#how-it-works)
- [Setup](#setup): [1. Tailscale policy](#1-tailscale-policy), [2. Auth key](#2-auth-key), [3. Run it](#3-run-it)
- [A project with the key server alone](#a-project-with-the-key-server-alone)
- [Onboarding a teammate](#onboarding-a-teammate)
- [Removing someone and rotating a level](#removing-someone-and-rotating-a-level)

## How it works

The key server keeps each level's private key and is reachable only on the tailnet, through `tailscale serve`, which tells it who is calling and which levels the Tailscale policy grants them. The CLI fetches the keys it needs into memory for one command and never writes them to disk.

```
                 tailnet (WireGuard)               your server
 keyko run ──────────────────────────▶ tailscale serve ──▶ keyko-server
   │  GET /v1/keys/myapp?levels=dev     adds Tailscale-User-Login      127.0.0.1:8787
   │                                        and Tailscale-App-Capabilities  │ keys from env
   ▼                                                                         │
 sops decrypt secrets/dev.env  ◀── SOPS_AGE_KEY (in sops's environment only) ┘
```

It takes level private keys from its environment, one variable per project and level:

```
KEYKO_KEY__MYAPP__DEV=AGE-SECRET-KEY-1...
KEYKO_KEY__MYAPP__PRODUCTION=AGE-SECRET-KEY-1NEW...,AGE-SECRET-KEY-1OLD...
```

Names are uppercased, and dashes become underscores. A variable can hold several keys, newest first, so branches still encrypted for an older key keep opening during a rotation.

It trusts the identity headers that `tailscale serve` sets (and strips from incoming requests), so **nothing but `tailscale serve` may reach it**. It listens on `127.0.0.1:8787` and refuses a non-loopback address unless `KEYKO_ALLOW_NON_LOOPBACK=1`.

| Variable | Default | |
| --- | --- | --- |
| `KEYKO_LISTEN` | `127.0.0.1:8787` | |
| `KEYKO_CAPABILITY` | `github.com/pokedotdev/keyko/cap/keyko` | The app capability it reads grants from. |
| `KEYKO_ACCESS_FILE` | none | Fallback grants by login, for when app capabilities cannot be forwarded ([below](#without-app-capabilities)). |
| `KEYKO_KEY__<PROJECT>__<LEVEL>` | | Level private keys. |

| Endpoint | Returns |
| --- | --- |
| `GET /v1/keys/<project>?levels=a,b` | The granted subset of those level keys, or 403. |
| `GET /v1/access/<project>` | The levels the caller may fetch, without keys. |
| `GET /healthz` | |

Each request is logged to stdout with the caller's login, the project, and the levels given and denied; never a key.

## Setup

### 1. Tailscale policy

App capabilities need Tailscale 1.92 or later on the server node. In the admin console, under **Access controls**, merge [`deploy/policy.hujson`](../deploy/policy.hujson): groups for each project's people, `tag:keyko` for the server, and grants such as

```jsonc
{
  "src": ["group:myapp-dev"],
  "dst": ["tag:keyko"],
  "ip": ["tcp:443"],
  "app": {
    "github.com/pokedotdev/keyko/cap/keyko": [
      { "projects": { "myapp": ["dev", "preview"] } }
    ]
  }
}
```

- Grants add up across groups.
- `"*"` stands for every project or every level the server holds (`{"projects": {"*": ["*"]}}`); use it sparingly.
- Tagged devices (servers, CI runners on the tailnet) carry no login but can be granted capabilities the same way; they show up as `tagged-node` in the log.

### 2. Auth key

Under **Settings > Keys**, generate an auth key: reusable off, ephemeral off, tags `tag:keyko`. The sidecar setups below need it only for the node's first login. Its state volume keeps it logged in, after which you can delete the key from the server's environment.

### 3. Run it

Pick whichever fits your hosting. In every case, check from a machine on the tailnet:

```bash
curl https://keyko.<tailnet>.ts.net/healthz
keyko whoami
```

If you use a different capability name, change it in `serve.json` (or `--accept-app-caps`), the policy and `KEYKO_CAPABILITY`. If MagicDNS names the node `keyko-1` (an old node kept the name), remove the old node or update `server` in each project.

#### Docker Compose with a Tailscale sidecar (recommended)

[`deploy/compose.yaml`](../deploy/compose.yaml) runs two containers that share one network namespace:

- `tailscale` joins the tailnet as the node `keyko` and runs `tailscale serve` from [`deploy/serve.json`](../deploy/serve.json): HTTPS on 443 with a tailnet certificate, proxied to `127.0.0.1:8787`, forwarding the `github.com/pokedotdev/keyko/cap/keyko` capability.
- `keyko` (built from the [`Dockerfile`](../Dockerfile): a static binary on an empty image, running as nobody) listens on `127.0.0.1:8787` inside that namespace. Neither the host nor other containers can reach it.

On any host with Docker:

```bash
git clone https://github.com/pokedotdev/keyko && cd keyko/deploy
cat > .env <<'ENV'      # mode 600; it holds private keys
TS_AUTHKEY=tskey-auth-...
KEYKO_KEY__MYAPP__DEV=AGE-SECRET-KEY-1...
ENV
chmod 600 .env
docker compose up -d --build
```

After changing `.env`, `docker compose up -d` restarts the server with the new keys. Any platform with sidecars or shared network namespaces works the same way, for example a Kubernetes pod with a Tailscale container and `TS_SERVE_CONFIG`.

#### On Dokploy

Create a **Compose** service from this repository with compose path `./deploy/compose.yaml`. In its **Environment**, set `TS_AUTHKEY` and the `KEYKO_KEY__...` variables; Dokploy writes them to `.env` beside the compose file, which the `keyko` container reads. Deploy.

With `[dokploy.key_server]` in a project's config, `keyko keygen --level` and `rotate-level` write the level keys there, and `--deploy` restarts it.

#### As a binary on a host already on the tailnet

Install `keyko-server-linux-<arch>` from a release, keep the keys in a root-only file, and run it bound to loopback, for example with systemd:

```ini
# /etc/systemd/system/keyko.service
[Service]
ExecStart=/usr/local/bin/keyko-server
EnvironmentFile=/etc/keyko/keys.env
DynamicUser=yes
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

Then publish it on the tailnet (the host's Tailscale must be 1.92 or later), and set `server = "https://<host>.<tailnet>.ts.net:8443"` in projects:

```bash
tailscale serve --bg --https=8443 --accept-app-caps=github.com/pokedotdev/keyko/cap/keyko http://127.0.0.1:8787
```

In the policy grants, `dst` is then that host (or its tag) and `ip` is `tcp:8443`. Any process on that host could reach the port directly and claim any identity, which the sidecar rules out, so prefer the sidecar on shared hosts.

#### Without app capabilities

If you cannot forward capabilities, mount a TOML file and point `KEYKO_ACCESS_FILE` at it. It maps `Tailscale-User-Login` values to grants; grants from both sources add up.

```toml
[users."alice@github"]
myapp = ["dev", "preview"]
```

## A project with the key server alone

A project can skip `[people]` and open every level through the key server. Write `secrets/config.toml` listing each level with an empty key until it is created:

```toml
project = "myapp"
server = "https://keyko.<tailnet>.ts.net"

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

Then create each level and put its private key on the server:

```bash
keyko keygen --level dev --out dev-key.txt
keyko keygen --level production --out production-key.txt
```

Put each file's last line in `KEYKO_KEY__MYAPP__DEV` and `KEYKO_KEY__MYAPP__PRODUCTION`, restart the server, and delete the files (with `[dokploy.key_server]`, `--yes --deploy` saves them there instead). Then:

```bash
keyko whoami                      # your tailnet login and the levels the server gives you
keyko set dev DATABASE_URL        # prompted, hidden; creates secrets/dev.env
keyko run dev -- pnpm dev         # the level key is fetched into memory for this run only
keyko check && git add secrets && git commit -m "secrets: add DATABASE_URL"
```

## Onboarding a teammate

1. A tailnet admin invites them and adds their login to the project's groups in the policy (for example `group:myapp-dev`).
2. They install Tailscale, log in with GitHub or Google, and install keyko (the project's Nix dev shell may already have it).
3. In the project:

   ```bash
   keyko whoami                    # login and the levels they get
   keyko run dev -- pnpm install   # secrets only in that process
   ```

Nobody rekeys anything. A new machine is the same: log in to Tailscale, run a command.

## Removing someone and rotating a level

For someone in `[people]`, `keyko people remove NAME` does it all (see [People](people.md#removing-someone)). For key server access, read on.

Removing someone from a group stops new fetches at once, but they may have kept the level keys they fetched. So rotate each level they had:

```bash
keyko rotate-level preview --out new-preview-key.txt   # any hosting
keyko rotate-level preview --deploy                    # with [dokploy.key_server]
```

`rotate-level`:

1. Generates a new key pair and saves the private key first, so a failure later loses nothing. With `--out FILE`, it goes to a new file for you to put on the key server (in front of the old key, comma-separated). With `[dokploy.key_server]`, it goes straight into the key server's Dokploy environment, redeployed with `--deploy`.
2. Re-encrypts every file of the level with new data keys (sops `rotate`, so old copies of the data key are useless) and the new public key.
3. Updates `secrets/config.toml` and `.sops.yaml`.
4. Lists the values the level could open. Rotate those at their source, because the old ciphertexts stay readable in git history.

Commit `secrets/`. Once no branch still needs the old key, remove it from the `KEYKO_KEY__...` variable.

To drop a machine or break-glass key: remove it from the config, then run `keyko revoke <age-pubkey>`.
