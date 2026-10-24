#!/usr/bin/env python3
"""B-STARTUP: QEMU, 3 warmups / 60 same-launch Ready and normal Exit samples.

Prepared inputs, 2 CPU / 128 MiB for VMs, warm caches; preparation and Python
startup excluded. Correct output and normal exit gate both metrics.
"""

from timing import main

if __name__ == "__main__":
    main("qemu")
