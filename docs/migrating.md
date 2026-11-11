# Migrating an existing project

This covers a project that uses SOPS with one age key per person (for example a script with `.age/key.txt` files and a hand-written `secrets/.sops.yaml`), and a tierkey 0.1 project that listed people under `[machines]`. Either can use personal keys, a key server, or both.

## To personal keys

1. List each person under `[people]` with their levels (see [People](people.md)), and remove them from `[machines]` and from the `machines = [...]` of `[[files]]` rules. Levels that only people open need no `[levels]` entry.
2. Run `tierkey rekey`, then `tierkey check`, and commit.

People's existing `.age/key.txt` keeps working, for agents too (`dev` only), since its public key is now under `[people]`. `TIERKEY_KEY_FILE` is no longer needed.

## To a key server

1. **Write `secrets/config.toml`.** Copy the path rules from `.sops.yaml` into `[[files]]` (in the same order) and the branch map into `[[branches]]`. Put the maintainer's existing public key in `break_glass`: their `.age/key.txt` becomes the break-glass key and keeps working (it is the default break-glass path). Move machine keys to `[machines]`. List each level with an empty key, `dev = ""`, until it is created; `check` reports them.
2. **Deploy the key server** with no level keys yet, and the policy grants (see [Key server setup](key-server.md#setup)).
3. **Create the levels**, with the maintainer's key. `tierkey keygen --level dev --out dev-key.txt` writes the level's public key into the config and its private key to a file. Put the file's last line in the server's `TIERKEY_KEY__<PROJECT>__DEV`, restart the server, and delete the file. With `[dokploy.key_server]`, `--yes --deploy` saves it there instead. Then `tierkey whoami` shows the levels you get.
4. **Re-encrypt.** `tierkey rekey` replaces `.sops.yaml` with the generated one and re-encrypts every file for its level, machines and break-glass key. Personal keys are no longer recipients.
5. **New data keys.** `rekey` only rewraps each file's data key, so a personal key that opened an old version in git history can still open the data key and, with it, values set later. For every personal key other than the break-glass one, run `tierkey revoke <age-pubkey>`: it gives the files it could open new data keys, and lists the values to rotate at their source if that person should lose access.
6. **Switch the tooling.** Replace the old script with a shim (`exec tierkey "$@"`), update the CI check to `tierkey check`, the build commands, and the docs. Delete personal `.age/` keys other than the maintainer's break-glass key.
7. Run `tierkey check`, and commit.
