import json
from pathlib import Path
import sys

count=int(sys.argv[1])
for i in range(count):
    (Path('files')/f'f{i:06d}').write_text(f'new-{i}\n')
print(json.dumps({'modified':count}))
