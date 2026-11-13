# Configuration

keyko looks for `secrets/config.toml` in the current directory or above it. `keyko init` writes a starting one.

## A full example

```toml
# secrets/config.toml
project = "myapp"                          # the project's name on the key server
server = "https://keyko.example.ts.net"  # the key server; KEYKO_SERVER overrides it
auto = "{env}/pages.env"                   # what `run auto` opens (the default)

# Maintainers' break-glass public keys: they open every file. The private keys stay offline.
break_glass = ["age1..."]

# Levels with a shared key. Public keys here, private keys on the key server.
[levels]
dev = "age1..."
preview = "age1..."
production = "age1..."

# People who open files with their own key, by level. `keyko people add` writes these lines.
[people]
alice = { key = "age1...", levels = ["dev", "ops"] }
bob = { key = "age1...", levels = ["dev"] }

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

`project` is required, and so is `[levels]` or `[people]` (or both). A project without a key server leaves out `server` and `[levels]`; one without per-person keys leaves out `[people]`.

## Levels

A level is whatever the `[[files]]` rules name. How it opens depends on where it appears:

| The level is in | It opens with | Needs the key server |
| --- | --- | --- |
| `[levels]` only | Its shared key, from the key server. | Yes |
| Someone's `levels` in `[people]` only (`ops` above) | The own key of each person given it. | No |
| Both | The person's own key for them; the key server for everyone else. | Only for people not given it |

Every level a rule uses must have a shared key or at least one person. A level listed with an empty key (`dev = ""`) is one not created yet, as during a [migration](migrating.md); `keyko keygen --level dev` creates it, and `check` reports it until then.

## Who each file is encrypted for

Each file is encrypted for, in this order:

1. its level's shared key, if it has one;
2. the people given its level;
3. the machines its rule lists;
4. the break-glass keys.

`keyko recipients` shows them for each file, labelled.

## `.sops.yaml`

`secrets/.sops.yaml` is generated from `config.toml` (`keyko rekey` writes it), so plain `sops` agrees with keyko. Do not edit it by hand; `keyko check` fails when it is stale.

## Targets

Commands take a target, which names a `.env` file under `secrets/`:

| Target | File |
| --- | --- |
| `dev` | `secrets/dev.env` |
| `<env>/<file>`, e.g. `production/api` | `secrets/production/api.env` |
| `auto` | The `auto` file of the current branch's environment. The branch comes from `CF_PAGES_BRANCH`, then the GitHub Actions branch, then git. |
| A path | A `.env` file under `secrets/`. |

## Optional: Dokploy

keyko works with any hosting. If yours is [Dokploy](https://dokploy.com), keyko can also push files to applications (`sync`, `check --remote`) and save new level keys straight to the key server (`keygen --level`, `rotate-level`):

```toml
[dokploy]
url = "http://my-server.example.ts.net:3000"  # DOKPLOY_URL overrides it
credentials = "ops.env"                       # holds DOKPLOY_API_KEY (the default)

[dokploy.apps]                                # files to Dokploy application IDs
"production/api.env" = "<applicationId>"

[dokploy.key_server]                          # where keyko-server runs
kind = "compose"                              # or "application"
id = "<composeId>"
```

Elsewhere, use `run` to hand the values to your platform's own CLI or deploy script, which read them from their environment (`keyko run production/api -- ./deploy.sh`), and place level keys with `--out FILE`.
