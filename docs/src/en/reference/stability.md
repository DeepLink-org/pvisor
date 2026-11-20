# Validate an integration

Use a representative task in a fresh Session to check the executor, installed
policy controls, output and failure behavior your pipeline depends on.

1. Check the required flags with `pvisor help COMMAND` and consult the
   [configuration reference](config.md) for TOML fields.
2. Read the [JSON result](json-output.md) and [Run Bundle](run-bundle.md) to
   confirm the outcome, selected executor and installed controls. Use the
   [record matrix](../design/records-and-versions.md) to locate receipts and
   their ownership bindings.
3. Run a selective apply and check that a conflicting host edit blocks a second
   apply. Keep the stage and ledger when the result requires recovery.
   [Review and apply](../guides/review-apply.md)
4. If your pipeline uses replay, run a recorded task and inspect the result
   with the configured adapter. [Replay](../guides/replay.md)
