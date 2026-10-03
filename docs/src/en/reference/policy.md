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
