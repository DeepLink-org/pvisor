# Policy model

Policy states what this execution may do. pVisor records distinct stages so requested permissions are not confused with installed controls:

```text
请求的权限 → 分层合并 → 准入（执行器能否提供） → 实际安装的控制 → 观察到的结果
```

## Sources

| Source | Location | Scope |
| --- | --- | --- |
| CLI | `--access`, `--overlaynet-*`, `--pass-env`, etc. | This run |
| Preset | `--safe`, `--ask` | Generates argument patches |
| Run config | `pvisor run --config run.toml` | This run |
| Session layer | `[policies.session]` in Run TOML | This Session |
| Workspace layer | `.pvisor/policy.toml` | Automatically loaded; narrows permissions |
| User layer | `~/.config/pvisor/policy.toml` or `$XDG_CONFIG_HOME/pvisor/policy.toml` | User defaults |

Argument precedence: **explicit CLI > safe preset > config file > ordinary defaults**.

## Layer merging

Session, workspace, user, and executor base policies intersect:

- **Network:** every declared layer must allow access. A deny, port/transport mismatch, or failed resolved-address check rejects it. Matching bandwidth limits stack. Omitted `default_action` denies unmatched targets.
- **Files:** choose the strictest decision: deny, ask, warn, allow. An allow cannot override another layer's restriction.
- **Interactive grants** cannot override explicit deny or base deny-all.
- Policy stays fixed during an Attempt; file edits affect later Sessions.

Policy files must belong to the current user, be unwritable by others, have no symlinks, and be at most 1 MiB. Unsafe inputs block startup. See [network](../guides/policies/network.md) and [file](../guides/policies/files.md) policies.

## Admission and degradation

Before startup an executor reports `Unsupported`, `Cooperative`, or `Planned` capability controls.

| Mode | Missing required control |
| --- | --- |
| Ordinary run | Best effort; missing controls recorded as warnings |
| `--safe` | Requires the selected executor's file-read, file-write, and network isolation; rejects startup instead of falling back |
| `--strict` | Requires non-bypassable evidence for every requested capability dimension; rejects before the agent starts |

`--safe` does not choose an executor. No current executor claims complete subprocess enforcement, so `--strict` currently rejects all executors and demonstrates fail-closed behavior.

## Installed controls and observations

Plans stop at `Planned`. Teardown returns `Unenforced`, `Cooperative`, or `Enforced` observations; only those supply enforcement evidence. See [capabilities and evidence](capabilities-and-evidence.md).
