# When you do not need pVisor

There is one test: **you are going to watch this execution from start to finish.**

- **You are watching it run**: approval and correction happen in the moment, so reviewing afterward adds nothing.
- **`git` already rolls it back cleanly**: if the diff is small and `git checkout` always leaves a clean tree, staging and selective merging do not save much.
- **The real side effects are external**: remote APIs, database writes, and sent messages are beyond staging's reach; rollback needs something else.
- **You need adversarial isolation**: for untrusted multi-tenancy you want a VM-grade isolation substrate, and pVisor is only one choice of executor.

When you want **unattended execution followed by review**, use pVisor to stage file changes, inspect execution records, and select the paths to apply.

For trade-offs against other tools see [comparisons](comparisons.md).
