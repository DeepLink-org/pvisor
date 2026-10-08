# File policies

File policies decide what the agent can see and change inside the workspace view. OverlayFS enforces them, so they apply only to access **through the workspace view**; original host paths outside the view are constrained by executor boundaries, see [executor boundaries](../../security/executor-boundaries.md).

## Default protection

`--safe` adds these rules by default:

| Level | Paths |
| --- | --- |
| deny | `.ssh` and `.gnupg` directories at any depth; `id_rsa`, `id_dsa`, `id_ecdsa`, `id_ecdsa_sk`, `id_ed25519`, `id_ed25519_sk` files |
| warn | `.env`, `.env.*`, `*.pem`, `*.key`, `*.pub`, `*.p12`, `*.pfx`, `.aws/credentials`, `.netrc`, `.npmrc` |

The preset recognizes only common filenames and cannot guarantee it detects every private key; add your own rules for custom-named secrets in a project.

## Add rules

```bash
pvisor run --safe \
  --access 'config/secrets/**:deny' \
  --access '**/.env*:warn' \
  -- agent-command
```

| Level | Effect |
| --- | --- |
| `deny` | The path is hidden from directory listings; access, creation, and modification are all denied |
| `ask` | Prompts on access (automatically enables the audit TUI and the `--safe` staging view) |
| `warn` | Allows access and prints the path to the supervisor's stderr (without contents); **not read-only** |

When multiple rules match, the strictest wins: deny > ask > warn. `--access` appends after the config file and preset and does not replace default protection; clearing default protection requires an explicit `--clear-access`.

## Matching rules

- Rules match relative to the mounted root: `*` does not cross directories, `**` does, and matching a directory covers all descendants.
- Matching is **case-insensitive**, preventing alias bypasses on case-insensitive filesystems.
- Absolute paths, empty rules, and path components containing `.` or `..` are invalid.
- With deny enabled, multiply hard-linked regular files and newly created hard links are conservatively denied; renaming, exchanging, or deleting a directory checks the affected subtree and refuses the operation when it contains a protected file.

## Read-only shares and direct writes

To let the agent read a directory outside the workspace, use an explicit share rather than a warn rule:

```bash
pvisor run --safe --mount /opt/tool:read -- agent-command
```

`--mount SOURCE:read` grants read-only access to the original absolute path and requires a host executor plus `--safe` or `--ask`; `SOURCE:write` writes host paths directly, bypassing staging and apply. Explicit shares do not pass through the workspace's ask rules, so do not share directories that contain secrets.

## Approval prompts

When an `ask` rule matches, the prompt lets you choose the grant scope: this file only, its sibling directory, or the same suffix; deny is selected by default. Choices are written to `audit-policy.json` in the current Job directory and applied automatically the next time the same scope matches, and each decision is recorded in `audit.jsonl`. Saved decisions apply to ask only and cannot override static deny or an outer sandbox.

## Layered policies

Besides the command line, policies can live in the workspace's `.pvisor/policy.toml` and the user's `~/.config/pvisor/policy.toml`, combined by taking the strictest decision across layers. See [policy model](../../concepts/policy-model.md).

For the full parameter set see [file access rules](../../reference/cli.md); for the TOML form see [the configuration model](../../reference/cli.md).
