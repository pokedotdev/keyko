# Migrating an existing project

This covers a project that uses SOPS with one age key per person (for example a script with `.age/key.txt` files and a hand-written `secrets/.sops.yaml`), and a tierkey 0.1 project that listed people under `[machines]`. Either can use personal keys, a key server, or both. It also covers a project from before keyko was renamed from tierkey.

## From tierkey

keyko was called tierkey up to 0.3. The files under `secrets/` are unchanged, and the old names keep working for now:

- **Your key.** `~/.config/tierkey/key.txt` is used while there is no `~/.config/keyko/key.txt`. Move it when you like: `mv ~/.config/tierkey ~/.config/keyko`.
- **Environment variables.** Each `TIERKEY_*` variable stands in for its `KEYKO_*` name, with a warning, unless that one is set. Rename them in CI, `.envrc` files and the key server's environment.
- **The key server.** On the default capability it reads grants of both `github.com/pokedotdev/keyko/cap/keyko` and `github.com/pokedotdev/tierkey/cap/tierkey`, so the tailnet policy and `serve.json` keep working. To move, add the new capability next to the old one in the policy grants and in `AcceptAppCaps`, deploy the new server, then remove the old one. `keyko keygen --level` and `keyko rotate` with `[dokploy.key_server]` save keys under `KEYKO_KEY__...`, keeping the older keys from `TIERKEY_KEY__...`; delete the old variable afterwards.
- **Invite codes** that start with `TIERKEY-INVITE-` still work with `keyko join`.

Then:

1. Install keyko (`cargo binstall keyko`, `npm i -D keyko`, or the release binaries) and replace `tierkey` with `keyko` in scripts, CI and docs.
2. Run `keyko check`. The header of `secrets/.sops.yaml` still names tierkey until the next command that writes it (`set`, `rekey`, ...); commit it then.

## To personal keys

1. List each person under `[people]` with their levels (see [People](people.md)), and remove them from `[machines]` and from the `machines = [...]` of `[[files]]` rules. Levels that only people open need no `[levels]` entry.
2. Run `keyko rekey`, then `keyko check`, and commit.

People's existing `.age/key.txt` keeps working, for agents too (`dev` only), since its public key is now under `[people]`. `KEYKO_KEY_FILE` is no longer needed.

## To a key server

1. **Write `secrets/config.toml`.** Copy the path rules from `.sops.yaml` into `[[files]]` (in the same order) and the branch map into `[[branches]]`. Put the maintainer's existing public key in `break_glass`: their `.age/key.txt` becomes the break-glass key and keeps working (it is the default break-glass path). Move machine keys to `[machines]`. List each level with an empty key, `dev = ""`, until it is created; `check` reports them.
2. **Deploy the key server** with no level keys yet, and the policy grants (see [Key server setup](key-server.md#setup)).
3. **Create the levels**, with the maintainer's key. `keyko keygen --level dev --out dev-key.txt` writes the level's public key into the config and its private key to a file. Put the file's last line in the server's `KEYKO_KEY__<PROJECT>__DEV`, restart the server, and delete the file. With `[dokploy.key_server]`, `--yes --deploy` saves it there instead. Then `keyko whoami` shows the levels you get.
4. **Re-encrypt.** `keyko rekey` replaces `.sops.yaml` with the generated one and re-encrypts every file for its level, machines and break-glass key. Personal keys are no longer recipients.
5. **New data keys.** `rekey` only rewraps each file's data key, so a personal key that opened an old version in git history can still open the data key and, with it, values set later. For every personal key other than the break-glass one, run `keyko revoke <age-pubkey>`: it gives the files it could open new data keys, and lists the values to rotate at their source if that person should lose access.
6. **Switch the tooling.** Replace the old script with a shim (`exec keyko "$@"`), update the CI check to `keyko check`, the build commands, and the docs. Delete personal `.age/` keys other than the maintainer's break-glass key.
7. Run `keyko check`, and commit.
