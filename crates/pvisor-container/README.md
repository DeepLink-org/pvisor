# pvisor-container

Shared host exec transport for `pvisor docker` and the containerd shim. Callers
supply a connected Unix socket and exec options, retaining endpoint discovery,
VM boot, storage and task events. `pvisor-guest` owns spawning and reaping.

`api` is the only public module. `ExecRuntime` starts and relays a single-use
session; dropping it or calling `ExecCancellation::cancel` cancels the exec.
Dropping only the cancellation handle does not cancel. Guest PIDs must never be
used for host signals. `relay` consumes an optional input File and borrows output
writers; EOF leaves output/cancellation available, and exit releases idle input
workers. `ExecOutcome` preserves code, signal and output completion;
`ExecRelayError` separates transport and host output failures. Streaming input
has no fixed write timeout; output writers own their backpressure policy.

Run `just test pvisor-container pvisor-guest pvisor-shim` for portable contracts;
Linux guest tests additionally exercise the launcher/reaper and control adapter.

## Frontend compatibility

The shared client reports facts; adapters retain their original behavior:

| Behavior | Docker CLI | containerd shim |
| --- | --- | --- |
| Environment and rlimits | Inherit launch values, apply explicit overrides | Replace with the complete OCI exec values, including empty collections |
| Guest session and cancellation | Inherit Docker's new-session policy; cancel the process group | Inherit bootstrap session; cancel only the exec process |
| Signal termination | Return 128 + signal | Return 255; explicit normal `exit 143` still returns 143 |
| Host startup timeout | Store socket's ten-second timeout | No fixed deadline |
| Guest output write/drain timeout | No fixed deadline | Two seconds; incomplete output returns 255 |
| Host output failure | Report an error | Return 255 |
| Lost connection without completion | Report an error | Return 137 |
| Concurrent control connections | Existing cap of 32 | Preserve the previous uncapped agent service |

Spawn rejection preserves the guest diagnostic and status (126/127 for execution
errors). The bounded JSON protocol replaces shim binary framing and Docker's
Ready handshake with Started/PID. Upgrade host and guest together and recreate
existing VM tasks; clients cannot attach to the old protocol.
