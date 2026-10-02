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

## Driver mode

| Mode | Executor | Boundary |
| --- | --- | --- |
| off | Any | OverlayNet disabled |
| proxy | Host/container | Cooperative proxy |
| auto | VM | Mandatory smoltcp data plane |

Without an explicit mode, network policy flags and Gateway capture infer the mode by executor.

## Policies

| Goal | Option | Other proxied traffic |
| --- | --- | --- |
| Allow specific destinations | --overlaynet-allow TARGET | Deny |
| Reject destinations | --overlaynet-deny TARGET | Allow |
| Reject ordinary egress | --overlaynet-deny-all | Deny; boundary above |
| Rate limit | --overlaynet-limit [TARGET=]RATE | Does not change authorization |

Allow/deny/limit are repeatable. Explicit deny wins. Deny-all is separate and cannot combine with other policy flags.

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

Host installs namespace/Seatbelt controls. Use container network none to block unproxied connections. VM auto rejects ordinary TCP egress. Deny-all still permits configured internal Gateway routes; disable Gateway and use VM off for complete offline execution.

Deny-all cannot have allow exceptions. To deny by default and allow a few destinations, specify an allowlist directly:

```bash
pvisor run \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-allow pypi.org:443 \
  -- agent-command
```

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

## Client coverage

Host/container inject upper/lowercase HTTP_PROXY, HTTPS_PROXY, and ALL_PROXY. Cooperative clients reach the proxy, which supports HTTP forwarding and HTTPS CONNECT.

Ordinary cooperative coverage excludes:

- Ignored/deleted proxy variables.
- NO_PROXY destinations.
- Direct sockets.
- DNS/UDP outside the HTTP proxy.

Such runs report safety.network_non_bypassable=false. Host deny-all creates Linux private netns or macOS Seatbelt external-IP/ambient-Unix-socket restrictions while retaining assigned loopback proxy, exact AgentCtl, and private Run IPC. Container none is another offline option. Selective ordinary host proxies stay cooperative; macOS safe adds direct-connection restrictions.

VM auto supplies static IPv4, synthetic DNS, and controlled TCP; off is offline. Gateway capture is an internal guest-router route. Container proxy needs host networking.

## Session, workspace, and user policy

CLI reads .pvisor/policy.toml from the workspace and $XDG_CONFIG_HOME/pvisor/policy.toml (default ~/.config/pvisor/policy.toml). Files contain network/filesystem tables. Explicit Run policies.session/workspace/user entries replace defaults for their corresponding network/filesystem layer.

Both directory and file must belong to the current user, have no symlinks, and be unwritable by others. Files must be regular and at most 1 MiB. Missing files add no constraints; unsafe paths/permissions/types/content block startup. Repository policy can only narrow permissions.

For example:

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

Every declared network layer and base policy must allow access. Omitted default_action denies unmatched targets; deny-only/rate-only layers need explicit allow. Any deny, port/transport mismatch, or failed resolution check rejects access. Matching rate limits stack. Interactive grants cannot override explicit deny or base deny-all. Files combine deny > ask > warn > allow. Globs are relative to the staged view. Attempt policy is fixed; changes affect later Sessions.

Network layers enable an explicit cooperative proxy for host/container auto. File layers create staging if absent. VM auto stays mandatory; off stays offline.

Embedding uses RunSpec.policies and must configure corresponding file/network drivers through PVisorBuilder; missing drivers reject execution.

## Inspect results

Each invocation preserves a separate Run associated with the current workspace:

```bash
pvisor run \
  --overlaynet-deny 169.254.0.0/16 \
  -- agent-command

pvisor status --review --json last | jq '{policy: .network.policy,
     interception: .network.interception,
     counters: .network.intercepted,
     non_bypassable: .safety.network_non_bypassable}'
```

Counters cover the driver's traffic only. They do not estimate cooperative-proxy bypass. Supported VM TCP/DNS has no guest bypass.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Allowed hostname resolves privately and is rejected | Explicit IP/CIDR or narrow allow_private_ips |
| Requests succeed under deny-all | Executor, installed controls, internal Gateway routes; container offline needs `--container-network none` |
| Proxy port cannot bind | Choose an unused nonzero port, for example `--overlaynet-listen 127.0.0.1:19082` |
| Container cannot reach proxy | Use --container-network host |
| VM proxy rejected | Use auto or off |

[Network examples](https://github.com/DeepLink-org/pvisor/tree/main/examples/pvisor/03-network-isolation) reproduce allowlist, deny-all, and direct-socket bypass offline.
