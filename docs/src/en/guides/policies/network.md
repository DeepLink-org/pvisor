# Control network access with OverlayNet

OverlayNet implements allow, deny, and bandwidth policies. Host/container runs use an in-process HTTP proxy; libkrun VM runs use an in-process smoltcp IPv4 TCP/DNS data plane. Read this with [capabilities and evidence](../../concepts/capabilities-and-evidence.md).

## Network boundaries {#网络边界}

| Path | Coverage | Direct egress |
| --- | --- | --- |
| Ordinary host/container explicit proxy | Proxied HTTP/HTTPS | Ignoring proxy, NO_PROXY, and direct sockets bypass it |
| Linux host deny-all | Private network namespace | Blocks direct IP egress; retains needed Run communication |
| macOS host deny-all | Seatbelt sockets | Blocks external IP/ambient Unix sockets; retains declared local communication |
| macOS host `--safe` | Seatbelt plus assigned loopback proxy | Blocks direct external connections; proxy enforces selective rules |
| Linux safe selective proxy | Supervisor loopback proxy | Cooperative; use VM or deny-all for mandatory networking |
| Container network none | OCI networking | Offline; incompatible with local proxy/Gateway requiring host networking |
| VM `--overlaynet auto` | smoltcp IPv4 TCP/DNS | No guest bypass; unsupported UDP/IPv6/ICMP/QUIC/inbound fails closed |
| VM off | No guest networking | Offline |

Check observations for the specific Run. File staging does not change networking; [evidence](../../concepts/capabilities-and-evidence.md) defines the interpretation.

## Allow declared destinations

Pass one or more allow flags before the agent command:

```bash
pvisor run \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-allow pypi.org:443 \
  -- agent-command
```

An allow rule enables OverlayNet and switches unmatched traffic to deny. Only the two listed HTTPS destinations are allowed through the proxy in this example.

## Choose a driver mode

Use `--overlaynet off|auto|proxy` as the main OverlayNet switch:

| Mode | Executor | Boundary |
| --- | --- | --- |
| `off` | Any | OverlayNet disabled |
| `proxy` | Host/container | Cooperative host proxy |
| `auto` | VM (recommended) | Mandatory smoltcp data plane |

Without an explicit mode, policy flags and Gateway capture infer the mode from the executor.

## Choose a policy

Policy flags configure the selected driver (inferred automatically when no mode is set):

| Goal | Option | Other proxied traffic |
| --- | --- | --- |
| Allow specific destinations | `--overlaynet-allow TARGET` | Deny |
| Reject destinations | `--overlaynet-deny TARGET` | Allow |
| Reject ordinary egress | `--overlaynet-deny-all` | Deny; boundary in the table above |
| Rate limit | `--overlaynet-limit [TARGET=]RATE` | Does not change allow/deny actions |

Allow, deny, and limit flags are all repeatable. An explicit deny takes precedence over allow. `--overlaynet-deny-all` is a separate policy and cannot be combined with other policy flags.

Targets accept exact hostnames, wildcard suffixes, IP/CIDR, and optional ports:

```bash
pvisor run \
  --overlaynet-allow '*.example.com:443' \
  --overlaynet-allow 203.0.113.10:443 \
  --overlaynet-deny 169.254.0.0/16 \
  -- agent-command
```

### Reject ordinary egress

```bash
pvisor run --overlaynet-deny-all -- agent-command
```

Host Runs install the namespace/Seatbelt network boundary from the table above; containers should use `--container-network none` to block connections outside the proxy. VM `auto` rejects ordinary guest TCP egress. Deny-all does not block a configured internal Gateway route; for complete offline execution, disable the Gateway and use `off` on the VM.

`--overlaynet-deny-all` does not support stacking allow exceptions. To default to deny-all while allowing a few addresses, declare the allowed targets directly instead of starting from deny-all: as soon as `--overlaynet-allow` appears, pVisor adopts an allowlist policy, allowing matching targets and denying other proxied targets by default.

### Bandwidth

Set a global limit and a stricter per-target limit:

```bash
pvisor run \
  --overlaynet-limit 10mbps \
  --overlaynet-limit api.openai.com:443=2mbps \
  -- agent-command
```

Matching limits stack; the effective rate is the strictest. kbps/mbps/gbps mean bits per second; kb/s/mb/s/gb/s mean bytes per second. Rate limits grant no access.

## Structured rules

Use TOML for multiple ports, transports, or intentional private-address resolution:

```toml
[run]
command = ["agent-command"]

[overlaynet]
mode = "auto" # VM 使用 smoltcp；host/container 使用 "proxy"
policy = "allowlist"

[[overlaynet.rules]]
host = "api.example.com"
ports = [443]
transports = ["tcp_tunnel"]
allow_private_ips = false

[[overlaynet.deny]]
host = "169.254.0.0/16"

[[overlaynet.limits]]
host = "api.example.com"
port = 443
bytes_per_second = 250000
```

Run:

```bash
pvisor run --config run.toml
```

Transports: http, https, tcp_tunnel. Empty ports/transports mean unrestricted in that dimension.

Hostname rules reject private/loopback resolution by default. Prefer narrow explicit IP/CIDR rules for private services, or narrowly scoped allow_private_ips=true. Link-local and other special ranges still need explicit IP/CIDR.

For host DNS/TUN fake-IP connectors, VM accepts 198.18/15 aliases only after logical hostname/port authorization. Guest literal-IP connections to that range remain blocked. These connectors hide the final real endpoint; use a resolver exposing concrete addresses when applying resolved IP/CIDR policy.

## Understanding which clients are controlled

For host/container Runs, pVisor injects `HTTP_PROXY`, `HTTPS_PROXY`, their lowercase forms, and `ALL_PROXY` into the agent process. HTTP clients that honor these settings go through OverlayNet; the proxy supports ordinary HTTP forwarding and HTTPS `CONNECT` tunnels.

The following paths are outside the boundary of an ordinary cooperative host/container proxy policy:

- A client that ignores or deletes the proxy environment variables.
- A destination added to `NO_PROXY`.
- A program that creates sockets directly.
- DNS and UDP traffic that does not go through the HTTP proxy.

A host/container cooperative-proxy Run therefore reports `safety.network_non_bypassable = false`. When you must block direct egress, use `pvisor run --overlaynet-deny-all -- COMMAND`: Linux creates a private network namespace; macOS uses Seatbelt to block non-loopback IPs and host ambient Unix sockets while retaining the loopback proxy, exact AgentCtl, and IPC inside private Run directories. Container Runs can also use `--container-network none`. The VM executor defaults to `[overlaynet] mode = "auto"`, where the guest uses a static IPv4 address and smoltcp provides synthetic DNS and policy-controlled IPv4 TCP; `mode = "off"` takes the VM offline. Gateway capture is exposed through the guest's virtual router; the container executor still requires `--container-network host` when it uses the in-process proxy.

## Session, workspace, and user policy

The CLI reads policy from the workspace's `.pvisor/policy.toml` and the user's `$XDG_CONFIG_HOME/pvisor/policy.toml` (default `~/.config/pvisor/policy.toml`); each file may contain a `[network]` and/or `[filesystem]` table. A Run TOML can set `[policies.session]`, `[policies.workspace]`, and `[policies.user]` explicitly; explicit network/filesystem entries replace the same layer's file defaults.

The policy directory and file must be owned by the current user and not writable by other users; neither may be a symbolic link, and the file must be a regular file no larger than 1 MiB. A missing file adds no policy; unsafe paths, permissions, file types, or invalid content block startup. Repository policy loads automatically but can only narrow permissions.

For example, a user allows a specific API and denies sensitive files:

```toml
[network]
allow = [{ host = "api.example.com", ports = [80, 443] }]

[filesystem]
deny = ["secrets/**"]
```

Further restrict the Session:

```toml
[policies.session.network]
allow = [{ host = "api.example.com", ports = [443] }]

[policies.session.filesystem]
deny = ["generated/private/**"]
```

Every declared network layer and base network policy must allow the request. An omitted `default_action` denies unmatched targets; a deny-only or bandwidth-limiting policy must set `default_action = "allow"` explicitly. Any explicit deny in a layer, a port/transport restriction, or a failed resolved-address safety check rejects the request, and matching bandwidth limits from every layer all stack. Interactive approval cannot override an explicit deny or a base deny-all. File policies combine across layers as deny > ask > warn > allow; allow cannot override another layer's restrictions. File globs are relative to the staged workspace view. Policy is fixed for the Attempt; changing the file only affects later Sessions.

Network layers enable an explicit cooperative proxy for host/container auto. File layers create staging if absent. VM auto stays mandatory; off stays offline.

Embedding uses RunSpec.policies and must configure corresponding file/network drivers through PVisorBuilder; missing drivers reject execution.

## Inspect results

The current directory is a reusable workspace by default; each invocation preserves a separate Run under pVisor's default record root:

```bash
pvisor run \
  --overlaynet-deny 169.254.0.0/16 \
  -- agent-command

pvisor status --review --json last | jq '{policy: .network.policy,
     interception: .network.interception,
     counters: .network.intercepted,
     non_bypassable: .safety.network_non_bypassable}'
```

These counters describe traffic handled by the current OverlayNet driver. They cannot count traffic that bypasses a cooperative host/container proxy; the VM smoltcp profile has no guest network bypass beyond its supported TCP/DNS data plane.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| An allowed hostname resolves to loopback or a private address and is still rejected | Use an explicit IP/CIDR rule, or set `allow_private_ips = true` only on a narrowly scoped structured rule |
| Requests still succeed under `--overlaynet-deny-all` | Check the executor, the controls actually installed, and internal Gateway routes; use `--container-network none` for an offline container |
| pVisor cannot bind the proxy port | Choose a free nonzero port with `--overlaynet-listen 127.0.0.1:19082` |
| A container cannot reach the proxy | Use `--container-network host` |
| VM `proxy` mode is rejected | Use `auto` to select smoltcp, or `off` to take the guest offline |

Run [`examples/pvisor/03-network-isolation`](https://github.com/DeepLink-org/pvisor/tree/main/examples/pvisor/03-network-isolation) to reproduce allowlist, deny-all, and direct-socket bypass offline.
