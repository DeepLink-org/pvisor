import json
import sys
from pathlib import Path

count = int(sys.argv[1])
for i in range(count):
    (Path("files") / f"f{i:06d}").write_text(f"new-{i}\n")
print(json.dumps({"modified": count}))
