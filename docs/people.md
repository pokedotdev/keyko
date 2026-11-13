# Personal keys

Each person keeps one age key for every project, and `[people]` in `secrets/config.toml` says which levels each person gets. Every file of those levels is encrypted for their key, so they open it with no server and no network, as with plain SOPS + age.

```toml
[people]
alice = { key = "age1...", levels = ["dev", "preview", "production"] }
bob = { key = "age1...", levels = ["dev"] }
```

## Compared with a key server

Personal keys need no server and no Tailscale: the encrypted files travel in the repository, and each teammate opens the levels they are given with their own key. Anyone who can pull the repository gets the files, but only the people given a level can read its values.

With a [key server](key-server.md), joining or switching machines is just logging in to the tailnet, with no commit. With personal keys, every addition, removal or new key is a `keyko people add` or `remove` that re-encrypts files, committed and pushed by someone who can open them; the others pick it up with `git pull`.

One project can use both (see [Mixing with level keys](#mixing-with-level-keys)).

## Your key

Your key is `~/.config/keyko/key.txt` (`$XDG_CONFIG_HOME/keyko/key.txt`). `KEYKO_KEY_FILE` points elsewhere, for example at a key you already use with sops. `keyko join` and `keyko init` create it, with mode 600, when you have none. It is the only copy: back it up, and never share or commit it.

## Starting a project

```bash
keyko init                        # secrets/config.toml, with you holding every level
```

It names the project after the directory (`--project` to choose) and you after git's `user.name` (`--name` to choose), and gives you `dev`, `preview` and `production`, with file rules and branches to match. Edit it to fit the project.

## Adding someone

There are two ways. An invite takes one command on each side; the other way sends nothing secret.

### With an invite

```bash
keyko people invite bob     # a maintainer: adds bob, commits and pushes secrets/, prints a code
keyko join                  # bob, in his clone: pastes the code; that is all
```

`people invite` makes a key for bob, adds him with it (`invited = "<date>"` in `[people]`), re-encrypts his levels' files, commits `secrets/` and pushes it. It prints a one-time code (`KEYKO-INVITE-...`): send it privately.

Bob's `keyko join` asks for the code (hidden). It pulls if the invite is not in his checkout yet, creates his own key if he has none, opens his files with the code, re-encrypts them for his key with new data keys, and commits and pushes `secrets/`. From then on the code opens nothing new, and a second use fails.

Until it is used, the code opens the files of the levels it gives, for whoever has it, and what it could read stays readable in git history. So an invite gives `dev` unless you pass `--any-level`; for other levels, prefer the way below. `keyko people` shows who has not joined yet, `keyko check` warns about invites older than 7 days, and `keyko people remove bob` cancels one.

Both commands refuse when `secrets/` has uncommitted changes, since they commit it. `--no-push` commits without pushing. When the push fails, for example on a protected branch, the commit stays: push it or open a pull request. Bob can work locally meanwhile.

### With their public key

```bash
keyko join                                   # they: press Enter at the code; prints the command below
keyko people add bob age1... --levels dev    # a maintainer: adds bob, re-encrypts dev files
```

`join` prints a ready-to-run `people add` line that holds only their public key, so it is safe to send over chat. Without `--levels`, someone new gets `dev`. The maintainer commits and pushes `secrets/`; the newcomer runs `git pull`, then `keyko whoami` to see their levels.

## Changing access

```bash
keyko people                                 # who has what
keyko people add bob --levels production     # more levels
keyko people remove bob --levels production  # take a level away
keyko people remove bob                      # take everything away
```

Whoever runs `people add` or `people remove` must be able to open the files involved, with their own levels, the key server or the break-glass key. If they cannot, nothing changes and the error says which level is missing. Commit and push `secrets/` afterwards; the others get access, or lose it, with `git pull`.

The last person with a level that has no shared key cannot be removed, since nobody could open its files; give the level to someone else first.

## Removing someone

`people remove` gives the files they could open new data keys and re-encrypts them without their key. They can still read old versions in git history, so rotate the values it lists at their source. If they also had key server access, rotate those levels too (see [the key server](key-server.md#removing-someone-and-rotating-a-level)).

## A new machine

Copy `~/.config/keyko/key.txt` over. Or run `keyko join` there and have a maintainer run `keyko people add bob <new key>`: that replaces the old key, gives its files new data keys, and lists the values it could open, to rotate if the old key may have leaked.

## Mixing with level keys

A level under `[levels]` that also has people opens with the person's own key for them, with no request to the key server, and through the key server for everyone else. A person who lacks a level still gets it from the key server if the Tailscale policy grants it.

To give a people-only level a shared key, run `keyko keygen --level L`, then `keyko rekey`.
