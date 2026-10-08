# 0003: Safe expresses requirements while executor selection stays explicit {#adr-0003}

**Status: implementation backfill.** This records the existing contract without inferring independent maintainer approval.

**Context.** Users choose executors for speed, Linux userspace, or kernel isolation while expecting their file and network policies to take effect.

**Options.** Safe could silently select an executor, or impose requirements on the chosen executor.

**Current choice.** `--safe` configures staging, file access, environment, and related settings without selecting an executor. Missing required controls cause startup rejection. See [Network policies](../../guides/policies/network.md) for network paths.

**Consequences.** Identical arguments can encounter capability errors on different machines. Install the prerequisites or explicitly select an executor that meets the requirements; reviewers inspect the Bundle for the outcome.
