# Design principles

PolicyVisor connects policy intent, runtime controls, and reviewable results. These principles keep that connection explicit.

## Boundaries are explicit

pVisor describes the execution boundary that was actually installed. It does
not silently upgrade a missing control into a stronger claim.

## Staged files are reviewed before application

With `--stage` enabled, workspace file changes remain staged until explicitly
applied. Review belongs before those changes reach the project. This does not
make remote API calls or other external side effects reversible.

## Evidence travels with the result

A summary should point back to the Run that produced it. Lineage is useful
only when it survives later inspection.

## Capture stays optional

pVisor can run without model-traffic capture. When capture is enabled, it is a
narrow handoff into the same Run, not a second product.

## Portable data beats a privileged viewer

Run records should remain inspectable through the CLI and documented formats.
A web view can improve discovery, but it should not be the only way to recover
an answer.

See the [system overview](index.md) and the [roadmap](../development/roadmap.md) for how
these principles shape current delivery.
