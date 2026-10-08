# Policy fields

Policies define which files a task can access and which destinations it can reach. Put shared project rules in `.pvisor/policy.toml`, personal rules in the user configuration directory, and task-specific restrictions in the Run configuration.

Start with the smallest useful grants, then check installed controls in the Run Bundle. The examples below cover rule syntax; the [network policy guide](../guides/policies/network.md) explains enforcement boundaries.

## Policy files available today

Use the following in workspace `.pvisor/policy.toml` or user-config `pvisor/policy.toml`. Run TOML uses the same structure under tables such as `[policies.session.network]` and `[policies.session.filesystem]`.

```toml
[network]
allow = [{ host = "api.example.com", ports = [443], transports = ["tcp_tunnel"] }]
deny = [{ host = "169.254.0.0/16" }]

[filesystem]
deny = ["secrets/**"]
ask = ["config/**"]
warn = ["**/.env*"]
```

| Field | Values and meaning |
| --- | --- |
| `network.default_action` | Optional `allow` / `deny`; omitted means deny. Deny-only/rate-only layers need explicit allow |
| `network.allow` / `deny` | Rule arrays; deny wins; every layer must allow |
| Rule `host` | Hostname, wildcard suffix, IP, CIDR; not a URL |
| Rule `ports` | Port array, no zero; empty is unrestricted |
| Rule `transports` | `http`, `https`, `tcp_tunnel`; empty is unrestricted |
| Rule `allow_private_ips` | Default false; hostname resolution to private/loopback addresses is denied; explicit IP/CIDR semantics in [network guide](../guides/policies/network.md) |
| `network.limits` | `host` and `port` may be omitted; `bytes_per_second` is bytes per second; matching limits stack |
| `filesystem.deny/ask/warn/allow` | Workspace-relative glob arrays; strictest decision; allow cannot override outer deny |

Policy files are not arbitrary trusted configuration: the directory and file must be owned by the current user, must not be group- or other-writable, and must be free of symlinks; the file must be a regular file no larger than 1 MiB. An unsafe policy blocks startup.

## From configuration to evidence

1. User, workspace, Session, and base policies intersect.
2. Admission describes intended controls, never above `Planned`.
3. Teardown supplies installed observations; `executor_observations` determines safety summaries.
4. Review rules, denials, and observation gaps together. No denials does not establish no violations.

Safe command-name presets and sensitive-file defaults are in [agent integration](../guides/agents/index.md) and [file policies](../guides/policies/files.md). See [CLI](cli.md) for grammar.


## Adding a restriction to one task {#scoped-example}

This Run allows the Session layer to pass traffic except metadata-address requests, caps intercepted traffic at 1 MiB/s, and blocks private project files. It still has to satisfy the base OverlayNet policy and any workspace/user policies.

```toml
[run]
command = ["/bin/sh", "-c", "printf 'ready\n'"]

[policies.session.network]
default_action = "allow"
deny = [{ host = "169.254.0.0/16" }]
limits = [{ bytes_per_second = 1048576 }]

[policies.session.filesystem]
deny = ["secrets/**"]
ask = ["config/**"]
warn = ["**/.env*"]
```

To use an allowlist instead, omit `default_action` and supply `allow` entries. For example, an allow rule for `api.example.com:443` in the Session layer grants nothing if the workspace layer denies that destination. With `default_action = "allow"`, a matching `deny` still wins. A rate-only layer without that default denies every unmatched destination; adding a limit does not imply permission.

## Loading and combining layers {#inheritance}

The CLI reads workspace `.pvisor/policy.toml` and the user configuration directory's `pvisor/policy.toml`. Missing files add no policy. Explicit `[policies.workspace.network]` replaces the entire automatically loaded workspace network dimension; it does not merge rule-by-rule. The filesystem dimension loads independently. The same behavior applies to `policies.user`.

An empty scoped `[network]` table is present and denies unmatched requests. An absent scoped network table adds no constraint for that scope. Do not interchange those two forms.

For a request to pass, the base network policy and every present scoped layer must permit it. All matching bandwidth limits are applied. File decisions use the most restrictive matching outcome across scopes: deny, then ask, then warn, then allow. `filesystem.allow` can constrain the policy's ordinary matching behavior but cannot cancel an outer scope's denial. See [File policies](../guides/policies/files.md) for how `ask` is resolved by each executor.

## File rule grammar {#file-globs}

The four user rule arrays are `deny`, `ask`, `warn`, and `allow`, all empty by default. Rules are case-insensitive, workspace-relative globs with literal path separators: `*` does not span a `/`; use `**` to span directories. An absolute path, NUL, empty path component, `.` component, or `..` component is invalid. Use `secrets/**`, not `/secrets/**` or `../secrets/**`.

Serialized filesystem policy records may also contain `context` and `layers`: these retain runtime binding and composed scope information. They are not additional grants to add to a normal policy file. The complete network entry fields and types are in [Configuration fields](config.md#all-fields).

## Safe and audit presets {#presets}

`--safe` and `--audit` build presets after loading TOML and before applying explicit CLI overrides. Both request staging, clear configured `pass_env`, and deny these sensitive paths:

```text
**/.ssh
**/.gnupg
**/id_rsa
**/id_dsa
**/id_ecdsa
**/id_ecdsa_sk
**/id_ed25519
**/id_ed25519_sk
```

Safe warns on the following matches; audit asks instead:

```text
**/.env
**/.env.*
**/*.pem
**/*.key
**/*.pub
**/*.p12
**/*.pfx
**/.aws/credentials
**/.netrc
**/.npmrc
```

The requested command's basename selects model destination presets:

| Command | Hostnames, port 443 |
| --- | --- |
| `codex`, `bash`, `sh`, `zsh`, `fish` | `api.openai.com`, `chatgpt.com`, `ab.chatgpt.com` |
| `claude` | `api.anthropic.com` |
| `gemini` | `generativelanguage.googleapis.com` |
| `zcode` | `api.z.ai`, `open.bigmodel.cn` |
| Other command names | Ordinary egress denied |

Capture with explicit Gateway routes also denies ordinary agent egress so model traffic uses Gateway. Explicit CLI destination options override presets; scoped policy restrictions still intersect them. The presets select `auto` networking for a VM and `proxy` for other executors. Use the [network guide](../guides/policies/network.md) to choose an executor that enforces your required boundary.
