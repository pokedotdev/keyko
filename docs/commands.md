# Commands

A `<target>` names a `.env` file under `secrets/`: `dev`, `production/api`, `auto` or a path (see [targets](configuration.md#targets)).

## Secrets

| Command | What it does |
| --- | --- |
| `run <target> [--optional] -- <cmd...>` | Runs `cmd` with the file's values in its environment; they exist nowhere else. With `--optional`, a missing file runs `cmd` without it. |
| `set <target> NAME [--random N]` | Sets a value: prompted (hidden), from stdin, or `N` random bytes as hex. Creates the file. |
| `unset <target> NAME` | Removes a key. |
| `names <target>` | Key names, without decrypting. |
| `edit <target>` | The decrypted file in `$EDITOR` (people only, not agents). |
| `env [branch]` | The environment a branch deploys to. |
| `pair list\|add\|remove <target> LIST [NAME [<target>:VAR ...]]` | Rotates one key at a time in a `name=key,...` list; `add` also sets the new key in each `<target>:VAR`. |

## Checks and maintenance

| Command | What it does |
| --- | --- |
| `check [--remote]` | Every file encrypted, for exactly its recipients; `.sops.yaml` current; no stray files; every environment mapped. Needs no key, so CI runs it. `--remote` also compares key names with Dokploy (if configured). |
| `recipients [target]` | The public keys that can open each file, labelled from the config. |
| `rekey` | Regenerates `.sops.yaml` and re-encrypts every file for its recipients. |
| `merge-fix <target>` | Re-signs a file after a hand-resolved git conflict. |

## People

| Command | What it does |
| --- | --- |
| `init [--name NAME] [--project P]` | Starts a project: `secrets/config.toml` with you in `[people]`, holding `dev`, `preview` and `production`. Creates your key if you have none. |
| `join [NAME]` | Creates your key if you have none, and prints the `people add` command a maintainer runs to add you. |
| `people` | Each person, their public key and levels. |
| `people add NAME [KEY] [--levels L,...]` | Adds someone (level `dev` unless told otherwise), gives them more levels, or replaces their key; re-encrypts the files they can now open. |
| `people remove NAME [--levels L,...]` | Removes someone, or only some of their levels: their files get new data keys and lose their key, and it lists the values to rotate. |
| `whoami` | Who you are in `[people]`, and who the key server sees you as and which levels it gives you. |

See [People](people.md).

## Keys and levels

| Command | What it does |
| --- | --- |
| `keygen [--out FILE]` | Creates a break-glass key (default `.age/key.txt` in the main checkout, shared by worktrees). |
| `keygen --level L [--out FILE] [--deploy] [--yes]` | Creates level `L`: its public key goes into the config, its private key to `--out FILE`, or to the key server's Dokploy service when `[dokploy.key_server]` is set. |
| `rotate-level L [--out FILE] [--deploy] [--yes]` | Replaces level `L`'s key: saves the new private key first, then re-encrypts every file of the level with new data keys, and lists the values the level could read. |
| `revoke <age-pubkey>` | After you remove a key from the config: rekeys, gives the files it could open new data keys, and lists the values to rotate. |

See [Key server](key-server.md#removing-someone-and-rotating-a-level).

## Dokploy

| Command | What it does |
| --- | --- |
| `sync <target> [--dry-run] [--yes] [--deploy]` | Saves a file as its Dokploy application's environment, keeping build args, build secrets and the env-file setting. Shows changes by name only. |

See [Configuration](configuration.md#optional-dokploy).

## Which key is used

For each command, tierkey picks the first of these that applies:

1. **`SOPS_AGE_KEY` or `SOPS_AGE_KEY_FILE`**, when set: machines (CI, build platforms).
2. **Your own key**, for the levels `[people]` gives you: `TIERKEY_KEY_FILE`, else `~/.config/tierkey/key.txt` (`$XDG_CONFIG_HOME/tierkey/key.txt`). A `.age/key.txt` in the main checkout counts as yours too when its public key is under `[people]`.
3. **The key server**, for the levels the command needs that your key does not cover. tierkey asks for exactly those, once per run, and passes them to `sops` in its environment. They never reach the command `run` starts, nor the disk.
4. **The break-glass key file**: `TIERKEY_BREAK_GLASS_KEY_FILE`, else `.age/key.txt` next to the main checkout. Used when there is no server, it cannot be reached, or it does not give a level you need. Commands that re-encrypt (`rekey`, `revoke`, `rotate-level`, `merge-fix`, `people`) also pass it alongside the other keys whenever the file exists, so files that are not yet encrypted for their level, as during a migration, still open.

Steps 2 and 3 combine: your key covers your levels, and the key server the rest.

sops runs with an empty config directory, so it never picks up a stray `~/.config/sops/age/keys.txt`.

### Reaching the key server

The key server's `*.ts.net` name is looked up with the system resolver first. If that fails, tierkey asks Tailscale's MagicDNS resolver (`100.100.100.100`) directly. The static Linux binary reads `/etc/resolv.conf` itself, so it misses the split DNS that systemd-resolved does for other programs when another VPN (Cloudflare WARP, for example) owns that file.

## Errors

Every error ends with what to do next, usually as a command to run, for example `run: tierkey join` or `ask a maintainer to run: tierkey people add bob --levels production`.
