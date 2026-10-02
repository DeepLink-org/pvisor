# When you do not need pVisor

There is one test: **you are going to watch this execution from start to finish.**

- **You are watching it run**: approval and correction happen in the moment, so reviewing afterward adds nothing.
- **`git` already rolls it back cleanly**: if the diff is small and `git checkout` always leaves a clean tree, staging and selective merging do not save much.
- **The real side effects are external**: remote APIs, database writes, and sent messages are beyond staging's reach; rollback needs something else.
- **You need adversarial isolation**: for untrusted multi-tenancy you want a VM-grade isolation substrate, and pVisor is only one choice of executor.

pVisor is for the opposite case: you **want to let the agent run unattended**. The goal is not to block every action but to move supervision from the process to the results—review the changes and evidence once at the end, and merge only what you want.

For trade-offs against other tools see [comparisons](comparisons.md).
