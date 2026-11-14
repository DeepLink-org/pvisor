# pvisor-overlaynet

**Network interception and egress-policy data planes for pVisor.**

Owns the proxy data plane: request classification, HTTP `CONNECT`,
absolute-URI forwarding, access enforcement through
[`pvisor-core`](../pvisor-core/README.md), request accounting,
shared proxy header safety, and dispatch to one caller-supplied `OverlaySink`.

CONNECT authorities accept a hostname or bracketed IPv6 host with an optional
nonzero `u16` port (443 when omitted). Only an empty suffix or `:port` may follow
the closing IPv6 bracket; trailing garbage is rejected with 400 before
authorization or upstream dispatch, rather than discarded into a default-port
target. Authorization and the connector use the same unbracketed host and
effective port.

Absolute-URI requests must also parse with the forwarding client's URL grammar
before authorization or sink selection. Invalid URLs, including overflowing
ports and garbage after a bracketed host, return 400 instead of being authorized
under a fallback port or delivered to an accepting sink. Legal URL spellings
(including an empty port or leading port zeroes) retain their existing defaults
and policy semantics; the `Host` header does not select the destination.
As with CONNECT, absolute IPv6 authorities lose only their enclosing brackets
before authorization and resolution. The bare literal resolves without DNS;
expanded/compressed spellings keep the same IP, port and HTTP/HTTPS transport
when the forwarding client canonicalizes the URL. Exact IP grants and explicit
address denies retain their existing policy meaning.

Each proxy admits at most 128 concurrent requests/tunnels; excess requests receive
429 without waiting in an unbounded queue. A permit remains owned by the HTTP body
or CONNECT tunnel and returns on completion/drop. This is a per-proxy limit, not a
node-wide memory budget. HTTP upstreams have a ten-minute read-inactivity timeout;
active responses can exceed ten minutes. CONNECT retains its existing transport
lifecycle and does not yet have a shared bidirectional inactivity deadline.
HTTP/1 and HTTP/2 CONNECT use Hyper's upgraded streams, not HTTP bodies, and
preserve successful half-close: the opposite direction drains
before tunnel ownership is released. A copy or download-resume error instead
cancels the companion direction and releases the tunnel's admission permit.

HTTP/2 CONNECT also observes an independently awaitable, latched reset/transport
failure signal from the pinned [local Hyper patch](../../vendor/hyper/PATCHES.md).
This releases both copies and the admission permit even while upload bandwidth,
upstream reads or native resume is pending, including resets after an earlier
half-close. Normal END_STREAM remains a half-close, not cancellation; delayed
responses and uploads after response EOF still drain. The reset monitor closes
after both halves finish or the upgraded owner drops. This is event-driven
transport cleanup, not an inactivity timeout or periodic write probe.

Also owns the libkrun VM driver: a non-bypassable virtio-net path whose
in-process smoltcp stack serves synthetic DNS, then terminates and
re-originates policy-authorized IPv4 TCP. The Rust guest supervisor configures
its static IPv4 address and route directly, without waiting for DHCP. Gateway capture is reachable through
the guest's virtual router; Gateway and ordinary VM egress share the Execution
controller, metrics, and bandwidth buckets.

The driver tracks at most `max_flows` concurrent TCP flows (default 1024;
each flow owns two 64 KiB socket buffers, so the cap doubles as the
worst-case memory bound). A flow whose handshake — SYN received, upstream
connect, guest ACK — does not complete within 30 seconds is aborted, even when
the upstream is already connected but the guest ACK is missing, and its
table slot reclaimed on the next pass, so bursts of short-lived connections
cannot pin the table until the idle timeout. A full table refuses new SYNs
(the guest sees `Connection refused`) and emits a rate-limited warning.
Synthetic DNS answers `localhost` from the guest's loopback address instead
of allocating a synthetic egress address.
DNS question names enforce the 63-octet label and 255-octet wire-name limits,
including the terminating root octet. Malformed questions receive FORMERR
without allocating a synthetic address. `tests/vm_dns.rs` sends a bounded
malformed/valid query trace through the public framed guest link, checking
truncation, length boundaries and continued service without host networking.
TCP SYN admission validates IPv4 and TCP headers with the stack's semantic
parsers before allocating a flow slot, including TCP options, nonzero ports
and flag combinations. Unsupported fragments and invalid IP versions cannot
reserve listeners. The same test fixture sends 15 deterministic malformed or
truncated SYN frames through a one-slot backend, checks uninterrupted UDP DNS,
and then requires an unrelated valid TCP DNS SYN to receive SYN-ACK. This
guards against rejected packets pinning the table and resetting healthy flows.

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

`tests/stream_recovery.rs` uses the public router and an accepting in-memory
sink, without opening sockets or contacting an upstream. A delayed body error,
a separately cancelled pending body and intervening healthy requests must retain
independent admission ownership. After cleanup, all 128 slots must be available
again, overload must return 429, and dropping the held bodies must restore service.
The ordinary case uses paused Tokio time. Its explicitly ignored real-timer
counterpart holds the delayed error for 90 seconds:

```sh
mise run test -- pvisor-overlaynet -- --test stream_recovery --run-ignored only \
  -E 'test(real_delayed_body_error_retains_admission_then_recovers)'
```

This checks router/body ownership and delayed local sink errors, not a real
HTTP transport, upstream read timeout or network reconnect.

## Links

- [OverlayNet architecture](../../docs/src/zh/design/overlaynet.md)
- [Network control](../../docs/src/zh/guides/policies/network.md)
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

`VmNetwork::start_with_io` runs the existing Ethernet/TCP/DNS data plane on a
caller-owned asynchronous duplex stream. Its factory runs inside the network
runtime, so Windows can adopt an IOCP byte pipe without a second packet engine.
Adoption errors are returned before startup succeeds; drop cancels and joins
the driver. Unix `start` retains its socket-pair convenience API and delegates
to the same implementation.

Windows peers use loopback UDP rather than Unix datagrams. The caller must
supply a canonical, guest-excluded caller/SYSTEM-only directory with inherited
private ACLs. Leased member records identify UDP endpoints and fresh 128-bit
generations. Every received packet checks current source membership and both
generation nonces; stale packets after address reuse are discarded. Frame
validation and subnet policy are shared with Unix. Transmit discovery may be
cached for 250 ms and may drop traffic during restart; it never authorizes
delivery to an old receiver generation. Queue/socket budgets remain bounded.
No UDP endpoint is exposed to guests or bound beyond host loopback.

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
each direction under deterministic loss, duplication and reordering.
The simulated guests retain partial nonblocking writes so full-duplex
backpressure cannot block their shared polling thread or bypass the test deadline.
The duplex recovery test drives simulated TCP time from socket readiness and the
next stack timer, rather than a multiple of host wall time. Before advancing a
timer, it gives real backend threads a bounded opportunity to deliver in-flight
frames; pending guest writes and retained fault frames prevent a time jump.
Regression checks cover partial incoming framing and backpressured output.
The test reserves the nextest execution pool while it runs. Its independent
20-second wall-clock watchdog, payloads and fault/integrity assertions remain
unchanged; this is a recovery/correctness test, not a throughput measurement.
Additional chaos tests exercise four-node concurrent forwarding with fragmented guest
stream writes, repeated restarts, saturation followed by receiver recovery, and
malformed-datagram floods. Congestion tests isolate a stalled guest from another
peer. These tests do not establish a real KVM/HVF guest boot or production capacity.
The fragmented ring test bounds each destination to one four-frame burst with
timed progress synchronization, including deliberate worker delays. It retains
all frame/byte/count assertions without assuming arbitrary lossless queueing.

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
reset that timer. Each backend turn consumes Tokio's cooperative budget so a
continuously readable peer socket still yields to the I/O reactor. This lets new
guest frames and timer wakes become visible during a flood. Shutdown can also
cancel a backpressured guest write. The peer transport still drops on congestion and retains per-destination isolation and all validation rules.

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
