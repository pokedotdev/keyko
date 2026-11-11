# People: one key per person

Each person keeps one age key for every project, and `[people]` in `secrets/config.toml` says which levels each person gets. Every file of those levels is encrypted for their key, so they open it with no server and no network, as with plain SOPS + age.

```toml
[people]
alice = { key = "age1...", levels = ["dev", "preview", "production"] }
bob = { key = "age1...", levels = ["dev"] }
```

## Sharing without Tailscale

Per-person keys need no Tailscale and no server: the encrypted files travel in the repository you already share, and each teammate opens the levels they are given with their own key. Anyone who can pull the repository gets the files, but only the people given a level can read its values.

Compared with [level keys](key-server.md), the difference is how access changes. Over Tailscale, joining or switching machines is just logging in. Here, every addition, removal or new key is a `tierkey people add` or `remove` that re-encrypts files, committed and pushed by someone who can open them; the others pick it up with `git pull`. For a small team that is usually enough.

A project that already uses the key server can add `[people]` too. Teammates open their levels with their own key, and need no Tailscale for those levels (see [Mixing with level keys](#mixing-with-level-keys)).

## Your key

Your key is `~/.config/tierkey/key.txt` (`$XDG_CONFIG_HOME/tierkey/key.txt`). `TIERKEY_KEY_FILE` points elsewhere, for example at a key you already use with sops. `tierkey join` and `tierkey init` create it, with mode 600, when you have none. It is the only copy: back it up, and never share or commit it.

## Starting a project

```bash
tierkey init                        # secrets/config.toml, with you holding every level
```

It names the project after the directory (`--project` to choose) and you after git's `user.name` (`--name` to choose), and gives you `dev`, `preview` and `production`, with file rules and branches to match. Edit it to fit the project.

## Adding someone

```bash
tierkey join                                   # they: their key, and the command below
tierkey people add bob age1... --levels dev    # a maintainer: adds bob, re-encrypts dev files
```

`join` prints a ready-to-run `people add` line that holds only their public key, so it is safe to send over chat. Without `--levels`, someone new gets `dev`. The maintainer commits and pushes `secrets/`; the newcomer runs `git pull`, then `tierkey whoami` to see their levels.

## Changing access

```bash
tierkey people                                 # who has what
tierkey people add bob --levels production     # more levels
tierkey people remove bob --levels production  # take a level away
tierkey people remove bob                      # take everything away
```

Whoever runs `people add` or `people remove` must be able to open the files involved, with their own levels, the key server or the break-glass key. If they cannot, nothing changes and the error says which level is missing. Commit and push `secrets/` afterwards; the others get access, or lose it, with `git pull`.

The last person with a level that has no shared key cannot be removed, since nobody could open its files; give the level to someone else first.

## Removing someone

`people remove` gives the files they could open new data keys and re-encrypts them without their key. They can still read old versions in git history, so rotate the values it lists at their source. If they also had key server access, rotate those levels too (see [the key server](key-server.md#removing-someone-and-rotating-a-level)).

## A new machine

Copy `~/.config/tierkey/key.txt` over. Or run `tierkey join` there and have a maintainer run `tierkey people add bob <new key>`: that replaces the old key, gives its files new data keys, and lists the values it could open, to rotate if the old key may have leaked.

## Mixing with level keys

A level under `[levels]` that also has people opens with the person's own key for them, with no request to the key server, and through the key server for everyone else. A person who lacks a level still gets it from the key server if the Tailscale policy grants it.

To give a people-only level a shared key, run `tierkey keygen --level L`, then `tierkey rekey`.
