# pVisor Hyper patch

Imported from the Cargo-verified `hyper` 1.9.0 crates.io release, SHA-256
`6299f016b246a94207e63da54dbe807655bf9e00044f73ded42c3ac5305fbcca`.
The upstream manifest, original manifest and MIT license are retained. The
normalized manifest adds a standalone workspace for the focused unit runner. Only
the published `src/` tree is imported; upstream examples/integration tests are
not part of the crates.io source archive.

## Independently observable upgraded HTTP/2 termination

`Upgraded::on_h2_disconnect()` is a local extension. Its optional, owned,
latched future resolves on a stream reset or transport failure. It returns
`None` for HTTP/1. It never treats normal END_STREAM as a disconnect, probes
the stream by writing, or changes read/write framing.

The HTTP/2 send bridge already polls `SendStream::poll_reset`. It now publishes
abnormal termination through a watch channel and continues monitoring after
local write shutdown until read EOF or owner destruction. Write shutdown is
acknowledged separately when END_STREAM is sent, so it does not await opposite
direction EOF. The monitor terminates normally once both halves complete.

OverlayNet selects this signal alongside its two copies, releasing both on
reset even during bandwidth or native resume waits. Its real Hyper regression
covers whole-tunnel drop, drop after upload/download EOF, throttled upload and
blocked resume, both legal half-close directions, and continued use of another
stream on the same connection. A raw h2 client additionally sends explicit
`CANCEL` and `NO_ERROR` resets before and after upload EOF. HTTP/1 retains its
existing behavior.

The root `[patch.crates-io]` must apply to every executable/fuzz workspace using
this extension so Axum, reqwest and the direct Hyper caller share one Hyper type
identity. This is not an upstream API or an assertion that a version-only
upgrade fixes the lifetime issue. Reassess the patch when updating Hyper.

## Validation

Project behavior is validated through `mise run test -- pvisor-overlaynet` and
`mise run test -- pvisor-gateway`, including real HTTP/2 reset and half-close
controls. The standalone vendored library's focused unit runner is:

```sh
mise exec -- cargo nextest run --manifest-path vendor/hyper/Cargo.toml \
  --lib --features full -E 'test(upgrade::tests::)'
```

Its generated standalone lockfile/target are not distributed. The upstream
crate's unrelated dev dependencies are used only by this special unit runner.
