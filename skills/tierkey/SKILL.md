---
name: tierkey
description: Use tierkey to work with a project's encrypted secrets (a secrets/ folder with config.toml and SOPS-encrypted .env files). Use it when a command needs secrets or environment variables, when adding, changing or removing a secret, when a secrets file has a git conflict, or when a build or check fails with a "tierkey:" error.
---

# tierkey for agents

Projects with `secrets/config.toml` keep their secrets in git, encrypted with SOPS + age. `tierkey` opens them with the user's own age key (people listed under `[people]`), with level keys from a key server on the tailnet, or both. Under Claude Code (`CLAUDECODE=1`, set for you) tierkey only opens `dev`-level files. So you can run with and change `dev`-level files, and you can prepare everything else for a person.

Every tierkey error ends with the command that fixes it. When that command is for the user ("ask the user to run it", "ask a maintainer to run"), give it to them verbatim instead of looking for another way.

## Rules

- Never print, echo, log or paste a secret value. Do not run `env`, `printenv`, `set`, `cat` or `sops` on secrets. Do not read `.age/`, `~/.config/tierkey/` or any key file.
- Only use secrets through `tierkey run`: the values exist only in that process.
- When a value is needed that you do not have, ask the user to run the `set` command themselves. Do not ask them to paste it in chat.
- Never write a decrypted file to disk, and never edit `secrets/*.env` or `secrets/.sops.yaml` by hand (the second is generated). The only hand edit is resolving key lines in a merge conflict, see below.

## Find your way

```bash
tierkey env                    # the environment the current branch deploys to
tierkey names dev              # key names in secrets/dev.env (no key needed, nothing decrypted)
tierkey names production/api   # works for any level: names are plaintext
tierkey whoami                 # who the user is in [people], and what the key server gives them
tierkey people                 # who opens which levels with their own key
```

A target is `dev`, `<env>/<file>` (`production/api` means `secrets/production/api.env`), `auto` (the build file of the current branch's environment), or a path under `secrets/`. `secrets/config.toml` says which level each file belongs to (`[[files]]`, first match wins) and which environment each branch deploys to (`[[branches]]`).

## Run with secrets (dev level)

```bash
tierkey run dev -- pnpm install
tierkey run dev -- cargo test
tierkey run dev -- sh -c 'some-tool --flag "$API_URL"'   # quote $VARS inside sh -c
```

Arguments pass through untouched. To test that a variable is set without printing it: `tierkey run dev -- sh -c 'test -n "$NAME"'`.

## Change dev-level secrets

```bash
tierkey set dev NAME --random 32               # a random secret; you never see it
printf %s "$NON_SECRET_VALUE" | tierkey set dev NAME   # from stdin, for values that are not secret
tierkey unset dev NAME
```

`set` creates the file and key if they are missing. Commit the changed `secrets/*.env` file: the diff shows the key name and nothing readable.

## Anything above dev: prepare it and hand over

You cannot open or change existing files of `preview`, `production` or other levels. Asking fails before any request, with `agents can only open dev-level files`. Do not look for a workaround. Instead:

1. Write the code, config and docs that use the new variable. Check names with `tierkey names <target>`.
2. Give the user the exact commands to run, for example:

   ```bash
   tierkey set production/api STRIPE_KEY            # prompts, hidden
   tierkey set preview/web SESSION_SECRET --random 32
   tierkey sync production/api --dry-run            # if the project pushes it to Dokploy
   ```

`edit`, `keygen --level`, `rotate-level` and `people add`/`remove` are refused for agents. Leave `rekey`, `revoke`, `sync`, `pair` and `init` to a person as well.

## Getting dev access

If tierkey says the user has no key, or is not in `[people]`:

1. Run `tierkey join`. It creates the user's key if there is none (you never see it) and prints a `tierkey people add ...` line.
2. Give the user that line: a maintainer of the project runs it, commits and pushes. Then `git pull` and check with `tierkey whoami`.

Do not edit `[people]` in `config.toml` yourself: adding someone also re-encrypts files, which `people add` does.

## Checks and conflicts

```bash
tierkey check                  # before committing anything under secrets/; needs no key
```

It fails on a plaintext value, a stray file under `secrets/`, a stale `.sops.yaml`, a file not encrypted for its recipients, or an environment directory missing from `[[branches]]`. Fix the cause it names. If a fix needs `tierkey rekey`, hand that to the user unless every file involved is `dev`.

On a git conflict in `secrets/<file>.env`, resolve the key lines like any conflict, keep either side's `sops_*` lines, then run `tierkey merge-fix <target>`. For files above `dev` that last step is the user's.

## Errors

| Message | Meaning, and what to do |
| --- | --- |
| `agents can only open dev-level files ...; ask the user to run it: tierkey ...` | Expected. Give the user that command. |
| `agents cannot ...; ask the user to run it: tierkey ...` | Same: `edit`, `keygen --level`, `rotate-level` and `people add`/`remove` are for people. |
| `level dev of <project> opens with personal keys, and you have none ...; run: tierkey join` | See [Getting dev access](#getting-dev-access). |
| `your key is not in [people] ...; ask a maintainer to run: tierkey people add ...` | Give the user that command. |
| `<name> does not have level dev ...; ask a maintainer to run: tierkey people add <name> --levels dev` | Same. |
| `... is not encrypted for <name> yet; someone who can open level ... must run tierkey rekey` | The config lists them but the file was not re-encrypted. `git pull` first; if it persists, ask the user. |
| `cannot reach the key server ... are you on the tailnet?` | The machine is not on Tailscale or the server is down. Ask the user; do not retry in a loop. |
| `the key server refused (403): ... may not have level dev` | The user lacks the grant. A tailnet admin must add them to the group. |
| `the key server refused (401)` | The request did not come through `tailscale serve`; the `server` URL in `config.toml` is probably wrong. |
| `secrets/x.env does not exist; tierkey set x NAME creates it` | Wrong target, or the file is new: `set` creates it. `run --optional` runs without it. |
| `matches no [[files]] rule` | `config.toml` has no level for that path. Ask before adding a rule. |
| `could not decrypt ...; if git merged it, run: tierkey merge-fix ...` | Key mismatch or a broken file after a merge. Run `merge-fix` for a `dev` file; otherwise tell the user. |
| `no secrets/config.toml ...; start one with: tierkey init` | Not a tierkey project (or the wrong directory). Ask the user before running `init`. |
