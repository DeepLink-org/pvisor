# Shared backing offload: engineering evidence

This directory accompanies the opt-in UKSM-style sharing/offload compatibility change. UKSM here means pVisor user-space physical page sharing, not kernel UKSM/KSM. These historical rollout replays are correctness and engineering observations, not published performance improvements or measurements of live model inference.

- [Validation and measurement scope](VALIDATION_NOTES.txt)
- [Full-precision three-arm results and per-trajectory outcomes](RESULTS.json)

The shared arm is slower and consumes more charged memory than offload alone. All three arms strictly completed 16 recorded trajectories (4 tasks × 4 rollouts) with matching official outcomes; only 3/16 fully pass official tests. Cache attribution limits whole-host memory comparisons. Excluding s15 is post-hoc and does not remove its original contention.

The integration feature is built on existing sharing; it does not implement kernel UKSM. Historical runtime backports and review-checkout tests are distinct evidence. No new replay was run for PR submission.
