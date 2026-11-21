# Adaptive directory prefetch for repeated negative lookups

## Motivation

Git history searches repeatedly probe for attributes files such as `.gitattributes` in working-tree directories. On gpu03, two Ansible replay commands exceeded the 30-second per-command timeout. Seven-second syscall samples recorded 15,075 and 26,157 failed opens of `.gitattributes`, respectively.

At [commit 064915b1](https://github.com/DeepLink-org/pvisor/blob/064915b1f38f3a0b37a71eb306ee232a3a5cc722/fw/patches/0044-fuse-adaptive-directory-prefetch.patch#L332-L335), repeated lookups of the same name do not advance the distinct-name prefetch trigger. A guest with a complete, valid directory listing can answer these negative lookups locally.

## Proposed change

For directories already eligible for the guest directory cache, count repeated lookups of the same name that return ENOENT. Once a bounded threshold is reached, attempt to import the directory using the existing prefetch budget. Answer a miss locally only while the listing is complete and valid. Fall back to the existing lookup path for partial or invalidated listings, and bound retries for large directories.

Reuse the existing invalidation paths for create, unlink, and rename. Preserve the eligibility checks for mutable host shares and snapshot workflows. Initially qualify the trigger with the existing logical directory cache setting enabled. Any change to the default can be considered separately.

## Evidence from the replay workload

Two fresh-VM measurements per configuration on gpu03 used 2 vCPUs and 5 GiB of guest memory. The explicit-prefetch configuration enabled the logical directory cache and enumerated 2,154 directories before running the queries.

| Query | Default configuration | After explicit prefetch |
| --- | ---: | ---: |
| b15, Git grep across up to 2,000 commits | 56.8–59.1 s | 4.1–4.4 s |
| b58, history search with two Git log pickaxe queries | 34.9–38.0 s | 16.7 s |

Prefetch added 2.6–2.7 seconds before the queries. Both queries produced identical output hashes across configurations. In separate full-trajectory runs with explicit prefetch, both queries finished within the original timeout and both trajectories reproduced their recorded patches.

In a single-directory probe, both configurations enumerated the parent before making 12,000 lookups of one absent name. Host LOOKUP counts across the entire probe instance, including startup, fell from 12,216 to 118 with logical directory caching enabled.

These measurements use explicit directory prefetch and establish a baseline for evaluating the proposed automatic trigger.
