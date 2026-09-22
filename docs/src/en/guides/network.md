# Control network access with OverlayNet

OverlayNet lets pVisor apply allow, deny, and bandwidth rules to network
egress. Host and container runs use its in-process HTTP proxy; libkrun VM runs
use an in-process smoltcp data plane for IPv4 TCP and DNS. Interpret these
controls with the [capability and evidence model](../concepts/capabilities-and-evidence.md).

!!! warning "Security boundary depends on the driver"
    The host/container explicit proxy is cooperative: a program can bypass it
    by removing proxy variables or opening a direct socket. VM `auto` is
    non-bypassable for the guest process tree because virtio-net terminates in
    pVisor. The VM MVP supports IPv4 TCP plus DNS; UDP, IPv6, ICMP, QUIC, and
    inbound forwarding fail closed.

## Allow only declared destinations

Pass one or more `--overlaynet-allow` options before the workload command:

```bash
pvisor run \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-allow pypi.org:443 \
  -- agent-command
```

The presence of an allow rule enables OverlayNet and changes the default
action to deny. In this example, intercepted traffic may reach the two listed
HTTPS destinations; other intercepted destinations are rejected.

## Choose a driver mode

Use `--overlaynet off|auto|proxy` as the primary OverlayNet switch:

| Mode | Executor | Boundary |
|---|---|---|
| `off` | Any | Disable OverlayNet |
| `proxy` | Host/container | Cooperative host proxy |
| `auto` | VM (recommended) | Non-bypassable smoltcp data plane |

If omitted, policy flags and Gateway capture infer the executor-appropriate mode.

## Forward through an existing proxy

When the server requires an existing HTTP proxy for egress, configure it explicitly:

```bash
pvisor run \
  --overlaynet proxy \
  --overlaynet-upstream-proxy http://127.0.0.1:17897 \
  --gateway-mode off --gateway-debug \
  -- agent-command
```

Traffic flows from the Agent through OverlayNet policy checks and recording, then
through the upstream proxy to the destination. pVisor does not inherit an ambient
upstream proxy: it replaces the Agent's proxy variables with its own listener.
Currently the upstream must use HTTP, a numeric IP, and no authentication. These
options require the explicit `proxy` driver. An
unavailable upstream produces an error without falling back to a direct connection.

If local DNS also returns unusable addresses, optionally select an HTTPS DNS JSON
service supporting A/AAAA queries, such as
`--overlaynet-dns-over-https https://dns.google/resolve`. This requires an upstream
proxy; DNS queries travel through that proxy, and the selected resolver receives
the queried hostnames. Omit the option to retain system DNS. pVisor authorizes the
hostname, checks the resolved IPs, then asks the upstream to CONNECT to an authorized
IP. The upstream does not re-resolve the destination hostname and bypass IP/CIDR
checks. DNS failures do not fall back to system resolution.

The TOML fields are `[overlaynet] upstream_proxy` and `dns_over_https`. Plain HTTP
also travels through a CONNECT tunnel to the authorized IP. HTTPS clients must
use standard CONNECT tunnels.

With `--gateway-debug`, the Run's `.capture/debug.log` includes `network.result`
entries with method, target authority, and response status. CONNECT 200 only means
the tunnel was established; it does not prove TLS or application success. These
entries do not expose HTTPS URL paths, bodies, or tool-call semantics. The Run
bundle's `network.intercepted` holds request, allow, deny, and handler-failure counts.
This proxy path does not update VM-specific DNS/TCP-flow/byte counters, or count
pVisor's own DNS queries as Agent requests. Traffic bypassing the explicit proxy
is outside these records, and network enforcement remains cooperative.

### Save Agent defaults once

`pvisor run -- codex` automatically reads `~/.config/pvisor/agents/codex.toml`.
If `XDG_CONFIG_HOME` is an absolute path, it replaces `~/.config`.
For the ChatGPT Gateway profile and an existing HTTP proxy, save:

```toml
[gateway]
profile = "codex-chatgpt"
level = "full"
debug = true

[overlaynet]
mode = "proxy"
upstream_proxy = "http://127.0.0.1:17897"
dns_over_https = "https://dns.google/resolve"
```

Set the proxy address for your environment; omit DoH if system DNS works.
After building and placing the desired pVisor binary on PATH, run from your project:

```bash
pvisor run -- codex
```

Defaults are selected by the command executable's basename and use the normal
RunConfig TOML schema. They are not loaded from the repository. Missing files leave
built-in defaults unchanged; invalid or unreadable files stop launch with an error.
CLI options override matching settings, subject to existing conflict validation.
`--spec` uses only the specified configuration; `--no-config` skips personal defaults.
Other agents are unaffected unless their own defaults file exists. Full capture
stores model request/response content; it does not enable stronger filesystem isolation.
An SSH reverse proxy must remain connected while the agent runs.

### Capture Codex model traffic through the upstream proxy

For Codex using ChatGPT authentication, build `pvisor` and run from the repository:

```bash
./target/debug/pvisor run \
  --gateway-profile codex-chatgpt \
  --gateway-level full --gateway-debug \
  --overlaynet-upstream-proxy http://127.0.0.1:17897 \
  --overlaynet-dns-over-https https://dns.google/resolve \
  -- codex
```

Replace the proxy address with your existing HTTP proxy. The DNS option is optional.
Append Codex arguments after `codex`, for example `-- codex exec "Reply OK without using tools"`.
The profile enables Gateway capture and infers the network driver; the example selects full capture.
It preserves native Responses requests and
model discovery, and selects HTTP/SSE for this Codex process only. Existing ChatGPT
authentication is reused; saved Codex configuration is not rewritten. This profile
does not enable WebSocket capture. It requires a direct `codex` executable and rejects
custom Gateway routes or an explicit `--gateway-mode off`. Existing capture levels
remain unchanged unless `--gateway-level` is supplied. The TOML equivalent is
`[gateway] profile = "codex-chatgpt"`. Omit the profile for API keys or custom providers.
The older Python helper delegates to this same built-in profile.

Gateway model requests now travel through OverlayNet's policy and DNS checks, then
through the configured upstream proxy. With an upstream proxy, all model route
upstreams must be HTTPS, and allowlists must permit their destinations. Failures do
not fall back to a direct connection. Explicit `wire_api="responses"` treats the
route upstream as the complete API base and avoids Chat Completions conversion.
One route may set `forward_models=true` to forward model discovery to that base.

The Run's capture events contain `llm.request` and `llm.response.stream` for model
HTTP/SSE traffic. Full capture records prompt and response content locally. Other
HTTPS traffic still provides CONNECT metadata only. Successful capture does not
establish filesystem isolation: check the Run's actual executor and boundary.

## Choose a policy

The policy options configure the selected driver (or infer one when no explicit
mode is supplied):

| Goal | Option | Behavior for other intercepted destinations |
|---|---|---|
| Allow only selected targets | `--overlaynet-allow TARGET` | Denied |
| Block selected targets | `--overlaynet-deny TARGET` | Allowed |
| Block all intercepted egress | `--overlaynet-deny-all` | Denied |
| Limit bandwidth | `--overlaynet-limit [TARGET=]RATE` | Unchanged |

Allow, deny, and limit options are repeatable. Explicit deny rules take
precedence over allow rules. `--overlaynet-deny-all` is a standalone policy and
cannot be combined with the other policy flags.

Targets accept an exact hostname, wildcard suffix, IP address, or CIDR, with
an optional port:

```bash
pvisor run \
  --overlaynet-allow '*.example.com:443' \
  --overlaynet-allow 203.0.113.10:443 \
  --overlaynet-deny 169.254.0.0/16 \
  -- agent-command
```

### Deny all intercepted traffic

```bash
pvisor run --overlaynet-deny-all -- agent-command
```

For host/container runs, this denies HTTP and HTTPS requests that reach the
injected proxy; it does not disable direct sockets or local Gateway routes. In
VM `auto` mode, the same policy denies ordinary guest TCP egress while the
internal Gateway route remains available when capture is enabled.

`--overlaynet-deny-all` does not support allow exceptions. If the intended
policy is “deny by default and allow only a few destinations,” do not start
with deny-all; declare the allowed targets directly:

```bash
pvisor run \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-allow pypi.org:443 \
  -- agent-command
```

The presence of `--overlaynet-allow` automatically selects the allowlist
policy: matching destinations are allowed and all other intercepted
destinations are denied by default.

### Limit bandwidth

Apply a global limit and a stricter target-specific limit:

```bash
pvisor run \
  --overlaynet-limit 10mbps \
  --overlaynet-limit api.openai.com:443=2mbps \
  -- agent-command
```

Matching limits stack, and the strictest effective rate applies. Rates ending
in `kbps`, `mbps`, or `gbps` are bits per second; `kb/s`, `mb/s`, and `gb/s`
are bytes per second. A limit constrains traffic but does not grant access.

## Use structured rules

Use a TOML configuration when a rule needs multiple ports, transport matching,
or intentional access to a private address resolved from a hostname:

```toml
[run]
command = ["agent-command"]

[overlaynet]
mode = "auto" # VM: smoltcp; use "proxy" for host/container
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

Run it with:

```bash
pvisor run --spec run.toml
```

Supported transport values are `http`, `https`, and `tcp_tunnel`. Empty
`ports` or `transports` mean unrestricted for that dimension.

Hostname rules reject private and loopback DNS results by default. For an
intentional private service, prefer an explicit IP/CIDR rule; alternatively,
set `allow_private_ips = true` on a narrowly scoped hostname rule. Link-local
and other special-purpose ranges still require an explicit IP or CIDR rule.

If the host uses a DNS/TUN fake-IP connector, VM egress accepts a `198.18/15`
result as an opaque connector alias only after the logical hostname and port
are authorized. A guest cannot connect to that range as an IP literal. The
connector does not expose the real final address, so use a concrete-address
resolver when IP/CIDR policy for hostname results is required.

## Understand which clients are controlled

For host and container runs, pVisor injects `HTTP_PROXY`, `HTTPS_PROXY`, their
lowercase forms, and `ALL_PROXY` into the Agent process. HTTP clients that
honor these settings are routed through OverlayNet. The proxy handles ordinary
HTTP forwarding and HTTPS `CONNECT` tunnels.

The following paths are outside that cooperative host/container boundary:

- a client that ignores or removes the proxy environment;
- a destination added to `NO_PROXY`;
- a program that opens a direct socket;
- DNS and UDP traffic that does not pass through the HTTP proxy.

Consequently, a host/container cooperative-proxy Run reports
`safety.network_non_bypassable = false`. When direct network access must be
blocked, use `pvisor -- --overlaynet-deny-all`: Linux adds a private
network namespace; macOS blocks non-loopback IP and ambient host Unix sockets with Seatbelt,
while retaining loopback proxy access and the exact AgentCtl and Run-local IPC. Container Runs can instead
use `--container-network none`. Selective allow/deny rules remain cooperative
on both native host paths. The VM executor defaults to `[overlaynet] mode =
"auto"`, which supplies DHCP, synthetic DNS, and policy-controlled IPv4 TCP;
`mode = "off"` leaves it offline. Gateway capture uses the guest virtual
router. The container executor still requires `--container-network host` for
the in-process proxy.

## Review the result

The current directory is the default reusable workspace. Each invocation keeps
an independent Run under pVisor's default records root:

```bash
pvisor run \
  --overlaynet-deny 169.254.0.0/16 \
  -- agent-command

pvisor review --json last | jq '{policy: .network.policy,
     interception: .network.interception,
     counters: .network.intercepted,
     non_bypassable: .safety.network_non_bypassable}'
```

The counters describe traffic handled by the active OverlayNet driver. They
cannot count traffic that bypassed the cooperative host/container proxy; the
VM smoltcp profile has no guest network path around its supported TCP/DNS data
plane.

## Troubleshooting

| Symptom | Check |
|---|---|
| An allowed hostname resolves to loopback or a private address | Use an explicit IP/CIDR rule, or a narrowly scoped structured rule with `allow_private_ips = true` |
| A request succeeds under `--overlaynet-deny-all` | Confirm the client honors the injected proxy and does not use `NO_PROXY` or a direct socket |
| pVisor cannot bind the proxy | Select a free non-zero address with `--overlaynet-listen 127.0.0.1:19082` |
| A container cannot reach the proxy | Use `--container-network host` |
| VM `proxy` mode is rejected | Use `auto` for the smoltcp driver, or `off` for an offline guest |

For an offline runnable walkthrough, use
[`examples/pvisor/03-network-isolation`](https://github.com/DeepLink-org/Persisting/tree/main/examples/pvisor/03-network-isolation).
For LLM request capture and model routing, continue with the
[Capture guide](capture.md).
