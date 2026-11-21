---
template: home.html
title: A container runtime optimized for agent workloads
description: A container runtime for agents with a Linux kernel per container, optional memory sharing across VMs, and lazy images that let tools start before a full download.
hide:
  - navigation
  - toc
---
## A container runtime optimized for agent workloads

Run containers with familiar Docker CLI commands. Review staged edits with pVisor's agent commands.

- [Stronger isolation](security/executor-boundaries.md): Each container has its own Linux kernel, keeping the agent's tools separate from host processes and other containers.
- [Lower memory use](benchmarks/vm-memory/index.md): With page sharing enabled, VMs running the same tools can use one copy of identical pages.
- [Faster tool startup](benchmarks/lazy-image-startup.md): Lazy images load content on demand, so an agent can start a tool before the whole image downloads.
