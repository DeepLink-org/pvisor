# File policies

OverlayFS controls what the agent can see/change **through the workspace view**. Executor boundaries govern original host paths outside it; see [boundaries](../../security/executor-boundaries.md).

## Default protection

`--safe` rules:

| Level | Paths |
| --- | --- |
| deny | .ssh/.gnupg at every depth; id_rsa, id_dsa, id_ecdsa, id_ecdsa_sk, id_ed25519, id_ed25519_sk |
| warn | .env, .env.*, *.pem, *.key, *.pub, *.p12, *.pfx, .aws/credentials, .netrc, .npmrc |

Name-based presets cannot detect every private key; add rules for custom secrets.

## Add rules

```bash
pvisor run --safe \
  --access 'config/secrets/**:deny' \
  --access '**/.env*:warn' \
  -- agent-command
```

| Level | Effect |
| --- | --- |
| deny | Hidden from listings; access/create/modify denied |
| ask | Interactive prompt; automatically enables audit TUI and safe staging |
| warn | Allowed with a stderr path warning, no contents; **not read-only** |

Strictest match wins: deny > ask > warn. Access appends after config/preset; clearing default protection requires `--clear-access`.

## Matching

- Relative to mounted root: * does not cross directories; ** does; directory matches include descendants.
- Case-insensitive to prevent aliases on case-insensitive filesystems.
- Absolute paths, empty rules, and dot/dot-dot components are invalid.
- With deny rules, multi-link regular files/new hard links are conservatively rejected. Rename/exchange/delete checks affected subtrees.

## Read-only shares and direct writes

Use explicit sharing for outside directories:

```bash
pvisor run --safe --mount /opt/tool:read -- agent-command
```

Read grants original-path access on host plus `--safe`/`--ask`. Write goes directly to the host outside staging/apply. Explicit shares bypass workspace ask rules; do not share secret-containing directories.

## Approval prompts

Choose one file, sibling directory, or matching suffix; deny is the default. Decisions go to audit-policy.json and audit.jsonl for the current Job. Saved decisions apply to ask only, never static deny or outer sandbox.

## Layers

Workspace and user policy files combine by strictest decision; see [policy model](../../concepts/policy-model.md). [CLI](../../reference/cli.md) owns parameters and TOML examples.
