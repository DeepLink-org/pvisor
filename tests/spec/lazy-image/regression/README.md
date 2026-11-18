# Lazy image resources and regressions

Follow the [feature README](../README.md) and [contract](../spec.md).
Product assertions live in the feature's native Scrut documents. This directory
owns only reusable resource setup and native fixture tests.

`fixtures.bash` isolates inherited cache/credential configuration, records its
source in the scenario evidence, and owns foreground loopback services as child
processes. The shared scenario fixture cleans their process group on every exit.
The TLS OCI input is copied from the existing CLI registry fixture rather than
reimplemented. All endpoints use ephemeral loopback ports and bounded readiness.

`repository.py` serves a private publication directory with exact single-range
GET responses and records requested object paths. It can reject all reads for a
controlled failure. Its unit tests independently check response bytes, ranges,
refusal and shutdown. It is a test service, not a cache implementation.
Package discovery includes these checks in `mise run test-py`; use the following
command for a focused run.

```sh
mise run test-py -- discover -s tests/spec/lazy-image/regression -p 'test_*.py' -v
```

Fixtures never create a public endpoint or use ambient AWS credentials. Do not
add production network prerequisites to these cases or turn missing resources
into expected semantic failures. Evidence belongs in the shared fixture's
owned temporary directory, not this source tree.
