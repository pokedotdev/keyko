# Agents

## What an agent may do

When Claude Code runs tierkey (`CLAUDECODE=1`), it only opens `dev`-level files:

- it only ever asks the key server for `dev`;
- it uses your own key for `dev` files only;
- it never uses the break-glass key;
- it refuses `edit`, `keygen --level`, `rotate-level` and `people add`/`remove`.

Anything else fails before a key is used, with the exact command for you to run, for example `ask the user to run it: tierkey run production/api -- ./deploy.sh`.

This is a guardrail, not a boundary: an agent with your shell could run the binary without the variable.

## How an agent gets `dev`

- **On your machine** it uses what you use: your key, or your tailnet login. Nothing to set up. If you have neither, the agent can run `tierkey join` and hand you the command it prints.
- **Without your key** (a cloud sandbox, a shared runner), give it a key of its own that only opens `dev`. That key opens nothing else, so this one is a boundary:

  ```bash
  TIERKEY_KEY_FILE=claude-key.txt tierkey join claude   # prints: tierkey people add claude age1... --levels dev
  tierkey people add claude age1... --levels dev        # then commit and push
  ```

  Set the last line of `claude-key.txt` as `SOPS_AGE_KEY` in the agent's environment, and delete the file.

## The agent skill

[`skills/tierkey/SKILL.md`](../skills/tierkey/SKILL.md) is an agent skill that teaches this workflow: run with `dev` secrets, set random values, check before committing, and hand anything above `dev` to a person as ready-to-run commands. Copy it into a project's `.claude/skills/tierkey/` (or your agent's skills folder):

```bash
mkdir -p .claude/skills/tierkey && cp path/to/tierkey/skills/tierkey/SKILL.md .claude/skills/tierkey/
```
