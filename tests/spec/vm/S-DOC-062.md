# S-DOC-062：N06 Seekable 增量 backing 回收与恢复


**语义**：启动时启用压缩，offload 返回合法 PVZRAM v2 manifest 与不可变 base/delta bundle；逻辑 RAM 大小完整，每次 offload 后整个 bundle 实际分配的磁盘空间均小于该测试负载的 RAM 范围。每次恢复后修改产生不同的 generation，其链可读取原文；十次回收、恢复覆盖从八层到一层的 compaction，不可达 generation 被回收，最多保留八层。guest 的固定数据 SHA-256 和持续更新的 1 MiB 页面内容在全部循环后均保持，暂停/恢复事件与状态一致。generation 不宣称跨设备的原子快照。

Linux 需要 FUSE；macOS 需要 macFUSE 内核后端。

**违反示例**：写入的只是普通 RAM 文件、缺失父层被当作零、manifest 未生成新的 head、Seekable 索引损坏、guest 缺页读回压缩字节而不是原文、回收后可变页面丢失，或 compaction 后旧层持续累积。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" vm S-DOC-062 --timeout 180 <<'CASE'
> require_vm_compression
> vm_case_setup
> vm_run_sdk compressed
> grep -Fq 'vm-control-case-ok compressed' "$CASE_ROOT/sdk.log"
> grep -Fq 'compressed-cycle=9 depth=1 layers=1 ' "$CASE_ROOT/sdk.log"
> grep -Fq 'compressed-cycle=10 depth=2 layers=2 ' "$CASE_ROOT/sdk.log"
> python3 - "$CASE_ROOT/startup.ram" <<'PY'
> import hashlib, json, pathlib, struct, sys
> manifest = pathlib.Path(sys.argv[1]).read_bytes()
> assert manifest[:8] == b'PVZRAM\0\0'
> n = struct.unpack_from('<I', manifest, 8)[0]
> descriptor = json.loads(manifest[12:12+n])
> assert descriptor['version'] == 2
> assert hashlib.sha256(manifest[12:12+n]).digest() == manifest[12+n:44+n]
> records = manifest[44+n:]
> assert len(records) >= 800 and len(records) % 80 == 0
> heads = []
> for i in range(0, len(records), 80):
>     record = records[i:i+80]
>     assert record[:8] == b'PVHEAD2\0'
>     assert hashlib.sha256(record[:48]).digest() == record[48:]
>     heads.append(record[16:48])
> assert len(set(heads)) >= 10
> root = pathlib.Path(descriptor['directory'])
> head, seen = heads[-1], set()
> while head is not None:
>     assert head not in seen and len(seen) < 8
>     seen.add(head)
>     with (root / (head.hex() + '.pvdelta')).open('rb') as f:
>         f.seek(-64, 2)
>         footer = f.read(64)
>         assert footer[:8] == b'PVSNAP2\0' and footer[32:] == head
>         offset, size, seek_size = struct.unpack_from('<QQQ', footer, 8)
>         assert offset + size + 64 == f.seek(0, 2)
>         f.seek(offset)
>         encoded = f.read(size)
>         assert hashlib.sha256(encoded).digest() == head
>         metadata = json.loads(encoded)
>         assert metadata['version'] == 2 and metadata['block_bytes'] == 65536
>         f.seek(offset - 9)
>         count, flags, magic = struct.unpack('<IBI', f.read(9))
>         assert flags == 0 and magic == 0x8F92EAB1 and seek_size == 17 + count * 8
>         f.seek(offset - seek_size)
>         magic, frame_size = struct.unpack('<II', f.read(8))
>         assert magic == 0x184D2A5E and frame_size == seek_size - 8
>         head = None if metadata['parent'] is None else bytes(metadata['parent'])
> assert {p.name for p in root.glob('*.pvdelta')} == {h.hex() + '.pvdelta' for h in seen}
> PY
> CASE
PASS S-DOC-062
```
