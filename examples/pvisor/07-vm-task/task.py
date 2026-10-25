"""Task executed inside the VM; replace this with your workload."""

import json
from pathlib import Path

inputs = json.loads(Path("input.json").read_text())
Path("result.json").write_text(json.dumps({"answer": sum(inputs["numbers"])}))
