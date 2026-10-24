# pvisor-overlaynet

**Network interception and egress-policy data planes for pVisor.**

Owns the proxy data plane: request classification, HTTP `CONNECT`,
absolute-URI forwarding, access enforcement through
[`pvisor-core`](../pvisor-core/README.md), request accounting,
shared proxy header safety, and dispatch to one caller-supplied `OverlaySink`.

Each proxy admits at most 128 concurrent requests/tunnels; excess requests receive
429 without waiting in an unbounded queue. A permit remains owned by the HTTP body
or CONNECT tunnel and returns on completion/drop. This is a per-proxy limit, not a
node-wide memory budget. HTTP upstreams have a ten-minute read-inactivity timeout;
active responses can exceed ten minutes. CONNECT retains its existing transport
lifecycle and does not yet have a shared bidirectional inactivity deadline.

Also owns the libkrun VM driver: a non-bypassable virtio-net path whose
in-process smoltcp stack serves synthetic DNS, then terminates and
re-originates policy-authorized IPv4 TCP. The Rust guest supervisor configures
its static IPv4 address and route directly, without waiting for DHCP. Gateway capture is reachable through
the guest's virtual router; Gateway and ordinary VM egress share the Attempt
controller, metrics, and bandwidth buckets.

Does not own LLM protocol adaptation, upstream selection, session correlation,
capture events, or Journal writes.
[`pvisor-gateway`](../pvisor-gateway/README.md) implements `OverlaySink`
for those. pVisor owns Run configuration, executor selection, and the recorded
interception profile.

The explicit-proxy profile is deliberately marked `cooperative`: policy
decisions over intercepted requests are not non-bypassable enforcement.
`no-network` and `allowlist` mean "for traffic that reached this proxy";
direct sockets and clients that remove proxy variables remain ambient.
`InterceptionProfile::vm_smoltcp()` records the VM's implemented non-bypassable
TCP/DNS surface. General UDP, IPv6, ICMP, QUIC, inbound forwarding, link-local,
and other reserved destinations fail closed. Linux-host netns and seccomp
drivers are not implemented.

## Develop

```bash
mise run test -- pvisor-overlaynet
```

## Links

- [OverlayNet architecture](../../docs/src/zh/design/overlaynet.md)
- [Network control](../../docs/src/zh/guides/network.md)
- [`pvisor-gateway`](../pvisor-gateway/README.md)
- [`pvisor-core`](../pvisor-core/README.md)

## Optional Host download waits

`OverlaySink::download_wait` defaults to no hook. After destination authorization,
GET forwarding and established CONNECT downstream reads may report `waiting`
and must await `ready` before delivering data, EOF or errors. The owning lifecycle
must fence and confirm native resume; transport silence alone is not guest idle.
Drop calls bounded synchronous `cancel`. HTTP/1 and HTTP/2 CONNECT preserve opaque
bytes with 16 KiB reads; upload remains active. This is Host buffering/backpressure,
not TLS termination or guest download execution. Disabled hooks retain forwarding.

## Same-host peer packet networks

`VmNetworkConfig.peer_network` explicitly joins a private directory-backed
network. The returned guest stream retains the same length-prefixed Ethernet
contract. Peer-bound frames are forwarded before smoltcp, directly to the
other member's Unix datagram socket; TCP remains end-to-end between guests.
Ordinary external traffic continues through the existing authorized smoltcp
connector. Packet forwarding does not require a broker, TAP, host bridge or
network-administration privileges.

The host runtime supplies a guest-excluded, canonical 0700 network directory.
Each usable host in a configured private IPv4 /24 owns one numeric slot containing
`lease.lock`, `peer.json` and `net.sock` (0600). The configured gateway is excluded
from membership. Legacy explicit links use `192.0.2.1` as router. Publication also
includes gateway and optional DNS aliases, so subnet mismatches fail closed.
The lifetime flock refuses live duplicates. After socket bind, an atomic record
publication exposes the protocol, instance identity, IP, MAC, fresh generation
and socket inode. A new owner holding the lease can replace a crashed owner's
stale publication. Shutdown removes its own socket and record; persistent slot
and lock files preserve lock-inode identity. No PID-based liveness decision is
used. Membership discovery is separate from the datagram packet link; IP-address
slots allow direct lookup without scanning the directory on every packet.
Positive and negative entries expire after 250 ms, so an address restart may drop packets
until refresh. Neither crash recovery nor delivery retries imply exactly-once
application effects.

The packet header fences source/destination generations. Receivers also check
the sender's socket pathname, membership lease, record/socket identity, and
frame source/destination IP and MAC. This is isolation from guests and other
host UIDs, not from a malicious process running as the same host UID. The
embedding caller must hide the whole directory from all guests. The pVisor
runtime places it below its existing guest-excluded host authority root.

Supported peer traffic is IPv4 unicast TCP, UDP and ICMP plus targeted ARP
requests/replies. Frames are capped at 1514 bytes (IP MTU 1500); VLANs, IPv6,
IPv4 fragments and general broadcasts/multicasts are dropped. The owning runtime
allocates addresses; the packet layer only enforces address leases. Nonempty
`PeerNetworkConfig.aliases` enables live member-name DNS from validated discovery
records, with zero TTL. An absent bare name returns NXDOMAIN; external domain names
keep their synthetic DNS path. Empty aliases disable peer DNS on that member. Malformed, absent-member and unsupported
peer destinations never fall back to host egress. Sends are nonblocking with no
application retry queue; missing/full peers drop packets rather than stalling
every VM flow. Each destination gets an independent, lazily bound `tx-<peer>.sock`
with a requested 16 KiB kernel send buffer (the OS may adjust accounting). There
are at most 252 outbound links per instance; this bounds descriptors, not total
process memory. Receivers validate this source pathname against membership.
Transmit sockets are removed before releasing the address lease and reclaimed
after crashes. A stopped guest can backpressure its own backend; remote senders
continue and may drop packets. Peer packet sent/received/dropped counts are
reported separately from proxy TCP flows and host-egress byte counters.
`InterceptionProfile.peer_ipv4` reports runtime-installed peer coverage; `udp`
still describes general host egress, which remains unsupported.

`VmNetwork::start` requires `EgressRuntime::for_peer_network`, which constructs
default-controller egress and rejects a restrictive policy or bandwidth budget.
The ordinary caller-supplied-controller constructor cannot admit peer membership. Live policy replacement closes both peer directions while
restricted, with already-admitted frames retaining ordinary in-flight effects.
The pVisor runtime additionally refuses custom controllers, host/container
executors and off/proxy modes: their packet policy semantics are not implemented.
Explicit membership grants all supported peer protocols to all network members.
It is not a per-peer or per-port ACL.

`tests/peer_network.rs` exercises the public owner/guest link and separate
processes, including ARP, TCP/UDP/ICMP forwarding, restart generations, duplicate
addresses, crash cleanup, policy changes and network isolation. A pair of guest
smoltcp stacks establishes TCP and verifies simultaneous 1 MiB transfers in
each direction under deterministic loss, duplication and reordering. Additional
chaos tests exercise four-node concurrent forwarding with fragmented guest
stream writes, repeated restarts, saturation followed by receiver recovery, and
malformed-datagram floods. Congestion tests isolate a stalled guest from another
peer. These tests do not establish a real KVM/HVF guest boot or production capacity.

The packet cache retains only the generation from validated discovery records;
the TTL and validation rules remain the same. Sends reuse bounded packet
buffers, and guest writes use vectored I/O with short-write handling. The
[peer engineering harness](https://github.com/DeepLink-org/pvisor/blob/555e750105e8072e0b85030dfb36226c66dae206/benchmark/peer/README.md) compares frozen
binaries through the public framed links with integrity-checked goodput, RTT
and process CPU accounting. Its backend-thread measurements exclude virtio/KVM
and do not represent Docker bridge or VM application TCP throughput.

Peer receive bursts and guest gather writes are capped at 32 frames per turn;
they drain only already-ready packets and never wait to fill a batch. Gather
writes preserve partial prefixes/payloads without concatenating frame contents.
Guest peer frames borrow the ingress parser buffer until routing completes,
avoiding an intermediate frame allocation. Ordinary smoltcp work runs on its
timer, control events and non-peer guest frames; peer-only traffic does not
reset that timer. Event selection remains fair under peer floods, with shutdown
also able to cancel a backpressured guest write. The peer transport still drops
on congestion and retains per-destination isolation and all validation rules.

On Linux, peer packet I/O uses `sendmmsg`/`recvmmsg` with at most 32 messages per
syscall. Outgoing frames are grouped by destination on its existing connected
socket and flushed at the end of the ingress read or when staging reaches 32;
there is no wait to fill a batch. Partial send progress advances by complete
datagrams, without resending accepted packets. WouldBlock still drops the
remaining packets for that destination, and healthy destinations continue;
there is no new congestion-retry queue or socket-budget increase. Other Unix
hosts retain single-message packet syscalls behind the same private interface.

Receive staging retains at most 32 bounded packets. Every packet independently
passes sender-path, generation, membership and frame validation before forwarding.
Truncated datagrams remain invalid even if their copied prefix looks valid.
Native receives participate in Tokio readiness clearing, so an exhausted socket
does not spin or lose its next wake. Guest stream framing and peer wire headers
are unchanged.

Peer bind requests a 64 KiB receive burst budget when the host default is
smaller, retaining larger existing defaults. This accommodates short MTU-sized
bursts on macOS, whose default Unix-datagram receive buffer is small. It is one
bounded kernel queue per instance, not a delivery or total-memory guarantee;
per-destination send budgets stay unchanged. A full receive queue may report
`ENOBUFS` on Darwin; packet sends treat it as transient congestion, drop the
remaining burst and retain the connected peer socket. Broken destinations still
invalidate their link. No global socket settings or application retries change.
