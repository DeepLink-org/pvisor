# offload 文件格式

[设计说明](index.md) · [文件内部布局 SVG](assets/overview.svg)

## 1. Motivation {#motivation}

保留一个 RAM 文件，需要知道它引用了哪些数据、哪些文件还在写、哪些历史数据可以删除。压缩模式下，这些信息分散在 manifest、generation 和 staging 中。本文给出磁盘目录、字节布局和大小公式，用于检查、容量规划和文件保留。

## 2. 核心设计 {#core-design}

manifest 保存目录和当前 head；generation 保存已提交的 RAM 块；staging 保存活动 writer 尚未提交的页。FUSE 将三者合成为 VM 可映射的逻辑 RAM 文件。文件树中的权限是创建时的权限，大小分别按逻辑长度和物理分配计量。

### 目录与文件职责

记号：`L`＝file-backed RAM 逻辑长度；`N`＝descriptor JSON 字节数；`k`＝追加 head 记录数；`P`＝压缩 frame 总字节数；`F`＝frame 数；`J`＝generation metadata JSON 字节数。

```text
普通模式（指定 /data/vm.ram）：
/data/
└── vm.ram                       [0600] 原始 RAM 字节
                                 逻辑大小 L；磁盘占用取决于写过的页/文件系统
/exports/
└── idle.ram                     [可选] hard link → vm.ram 同 inode
                                 st_size 同为 L；不新增第二份 RAM 数据

压缩模式（指定 /data/vm.ram）：
/data/
├── vm.ram                       [0600] manifest，逻辑大小 44 + N + 80k B
│                                存目录与 committed head，不存 RAM payload
└── vm.ram.layers/               [0700] generations 与活动 writer staging
    ├── .tmp<随机名>             [临时] staging，稀疏 RAM 页，非压缩
    │                            初始 0 B；长度≤L；commit 后 truncate(0)
    ├── <base-id>.pvdelta        [不可变] 完整 base：全部逻辑块的 entry
    │                            大小 P + (17 + 8F) + J + 64 B
    ├── <delta-id>.pvdelta       [不可变] delta：变化块 entry＋parent ID
    │                            同一大小公式；继承部分不重复保存
    ├── <历史-head-id>.pvpin     [可选，0600] 空文件，长度 0 B
    │                            文件名选保留根；GC 保留它及其 ancestors
    └── .tmp<捕获随机名>         [短暂] generation 写入中的临时文件
                                 从 0 增长；完成后 no-clobber 发布为 .pvdelta

<用户cache>/pvisor/ram/
└── mount-<随机名>/              [0700] 临时 FUSE 挂载目录
    └── ram                      [虚拟，0600] VM mmap 的逻辑 RAM
                                 逻辑大小 L；无独立完整 RAM payload 文件

/exports/
└── idle.ram                     [可选] hard link → vm.ram manifest inode
                                 大小同 manifest；layers 仍在 /data/vm.ram.layers
```

未指定 backing 时，上面的 manifest 改成 `<cache>/pvisor/ram/.tmp<随机名>`，sidecar 改成同一 cache 中 `layers-<随机名>/`。发布 alias 不会改名或移动该目录。普通模式也用 cache 临时文件，但没有 layers / mount。

权限描述为创建路径的意图：显式 regular file 为 0600，新目录为 0700；tempfile 用私有临时文件权限。已存在 cache 父目录的权限不会因 `create_dir` 自动修正。`.pvdelta` 由私有 tempfile 发布形成。

磁盘还可能有 runner 临时目录的 `runner.json`、attestation，以及 Run Journal / Bundle。这些是配置/证据文件，大小随业务配置与事件数变化，不属于 RAM 数据格式，也不由 manifest 引用。实验 pool socket / inventory JSON 见[主文档边界说明](index.md#experimental-integration-boundary)。

## 3. 关键数据和核心机制详细设计 {#detailed-design}

### 文件大小

#### 原始 RAM backing 与 FUSE 逻辑 RAM

VMM 将多个 RAM region 按主机页紧凑放入文件。设主机页为 H，则按当前 mapping 实现：

```text
L = Σ align_up(region_length_i, H)
```

RAM 已对齐的常见情况下，256 MiB file-backed RAM 对应 `L=268435456 B`。这个示例不是“任何 memory_mib=256 的平台都必然有完全相同布局”；guest 内存几何、设备窗口需按 VMM 实际范围判断。

普通文件 `set_len(L)` 可以产生稀疏区，磁盘占用随写入增长；硬链接 alias 不重复计费。FUSE `ram` 的 `FileAttr.blocks` 按 `ceil(L/512)` 报告逻辑块数，**不能拿它作为真实压缩磁盘占用**。

#### staging：按文件 offset 存原始页

staging 没有 header、JSON 或块压缩，其 offset 与紧凑逻辑 RAM offset 对应。部分页第一次写入时会从 parent 补齐其余字节，再落 staging；完整页写入可直接覆盖。

```text
staging byte offset = RAM logical byte offset
页 p 的范围         = [p × 4096, min((p+1) × 4096, L))
st_size             = 最高一次实际写入的结尾位置，初始/commit 后为 0
```

写最后一页可以让 `st_size` 接近 L，而物理只分配少量块；脏页累计很多时占用也可能接近 L。脏页掩码在 `BTreeMap<u64,u16>` 内存结构中，不在 staging 文件里，所以单独找到 staging 不足以可靠恢复未提交 RAM epoch。FUSE 脏页尚未写回时，它还可能不在 staging 中。

#### generation：压缩率、entry 数和 frame 数分别影响大小

```text
S_seek       = 17 + 8F
S_generation = P + S_seek + J + 64
```

P 是逐 frame 压缩后的字节总和；fill 块没有 frame。完整 base 的 entry 数等于 layout 的逻辑块数；delta 的 entry 数取决于内容真的变化的块数，和 dirty 页数不同。均匀块虽然 P≈0，也仍需 entry/checksum JSON，不能说零块完全不占空间。

J 最大 64 MiB；JSON 将 32 字节 ID/checksum 写成整数数组，会有明显文本开销。L 的准入上限 64 GiB 不保证其 base index 总能放进 64 MiB（VMR-06）。不可压缩 payload 也会增加编码/索引开销，不能保证 `.pvdelta` 小于原始 RAM。

合并瞬间可能同时存在旧链、新全量 base、capture 临时文件、staging 与 pin 保留历史链。空间峰值应按这些资源求和，不能只看 current head 的 delta 大小。目录本身也有文件系统占用。

#### manifest 与硬链接

```text
S_manifest = 8 + 4 + N + 32 + 80k = 44 + N + 80k
```

N 最大 8192，k 的读端预算最大 1,000,000。manifest 不会随着 RAM L 直接线性增长；它随路径长度和提交历史增长。没有变化的 generation 可以复用 head，因此“一次 offload”不必等于“一条新 head”。硬链接 path 没有第二份 S_manifest；统计应按 inode 去重。

### manifest：二进制 envelope＋JSON descriptor

![manifest 字节布局、head record 与 magic 逐字节展开](assets/manifest-layout.svg)

#### 精确字节布局

| offset | 长度 | 字段 | 编码 |
|---|---:|---|---|
| 0 | 8 | magic | `PVZRAM\0\0`，其中 `\0` 为一个 NUL 字节 |
| 8 | 4 | descriptor length N | u32 little-endian |
| 12 | N | descriptor | UTF-8 JSON，紧凑编码，无缩进 |
| 12+N | 32 | descriptor checksum | SHA-256(JSON **原字节**) |
| 44+N | 80k | head log | 固定长度 records，按追加顺序 |

descriptor 实际例子：

```json
{"version":2,"directory":"/data/vm.ram.layers"}
```

近似 JSON Schema（只描述嵌入的 descriptor，不是整个 manifest 文件）：

```json
{
  "type": "object",
  "additionalProperties": false,
  "required": ["version", "directory"],
  "properties": {
    "version": { "const": 2 },
    "directory": { "type": "string", "description": "宿主绝对 layers 目录" }
  }
}
```

`directory` 必须是平台认可的绝对路径，整个 JSON 序列化后不超过 8192 字节；这些条件由 Rust 校验，不由上面的基础 schema 独自保证。描述字节重新格式化会改变 checksum，不能直接把 JSON 段改成 pretty JSON 后继续使用旧校验。

#### 每条 head record：80 B

| record 内 offset | 长度 | 字段 |
|---|---:|---|
| 0 | 8 | `PVHEAD2\0` |
| 8 | 8 | logical RAM bytes，u64 LE |
| 16 | 32 | head ImageId，原始 32 字节；目录文件名是其 64 位 hex |
| 48 | 32 | SHA-256(record[0:48]) |

head log **不是 JSON**。可把一条记录理解成以下语义对象，但磁盘不这样序列化：

```text
HeadRecord { magic, logical_bytes: u64, head_id: [u8;32], checksum: [u8;32] }
```

各 head record 的 logical RAM length 必须一致。GC 不把旧 records 自动视作 pin，所以旧 head 可能指向已删除 generation；只读 open 从最后 head 建立当前 chain。

### `.pvdelta`：payload＋seek table＋JSON index＋footer

![generation 字节布局、seek table 和 footer 展开](assets/generation-layout.svg)

#### offset 与 footer

设 `I=P+(17+8F)`，即 metadata 起始 offset：

```text
[0, P)             Zstd frames
[P, I)             seekable table，17+8F 字节
[I, I+J)           metadata JSON
[I+J, I+J+64)      footer
```

footer 内部：

| footer 内 offset | 长度 | 字段 |
|---|---:|---|
| 0 | 8 | `PVSNAP2\0` |
| 8 | 8 | metadata 起始 offset I，u64 LE |
| 16 | 8 | metadata JSON 字节数 J，u64 LE |
| 24 | 8 | seekable table 字节数，u64 LE |
| 32 | 32 | ImageId＝SHA-256(metadata 原字节) |

reader 从 EOF 回退 64 B 读 footer，再定位 index。seek table 是 skippable magic/length（8）＋每 frame stored/decoded size（8F）＋frame count（4）＋flags（1）＋seek footer magic（4）。当前 seek checksum flag=0，RAM block 校验由外层 SHA-256 承担。

#### metadata schema：与 Rust 字段对应

以下是 **writer 正常输出的近似 schema**；writer 会显式写 `parent/frame/fill` 的 null。Serde 对 Option 缺省的接收行为可能更宽松，且跨字段、checksum 和继承约束仍由 Rust 校验。

```json
{
  "$defs": {
    "ImageId": {
      "type": "array", "minItems": 32, "maxItems": 32,
      "items": { "type": "integer", "minimum": 0, "maximum": 255 }
    },
    "Region": {
      "type": "object", "additionalProperties": false,
      "required": ["guest_address", "length"],
      "properties": {
        "guest_address": { "type": "integer", "minimum": 0 },
        "length": { "type": "integer", "minimum": 1 }
      }
    },
    "Entry": {
      "type": "object", "additionalProperties": false,
      "required": ["block", "frame", "fill", "checksum"],
      "properties": {
        "block": { "type": "integer", "minimum": 0 },
        "frame": { "type": ["integer", "null"], "minimum": 0 },
        "fill": { "type": ["integer", "null"], "minimum": 0, "maximum": 255 },
        "checksum": { "$ref": "#/$defs/ImageId" }
      }
    }
  },
  "type": "object", "additionalProperties": false,
  "required": ["version", "block_bytes", "parent", "layout", "entries"],
  "properties": {
    "version": { "const": 2 },
    "block_bytes": { "const": 65536 },
    "parent": { "anyOf": [{ "type": "null" }, { "$ref": "#/$defs/ImageId" }] },
    "layout": {
      "type": "object", "additionalProperties": false,
      "required": ["page_bytes", "regions"],
      "properties": {
        "page_bytes": { "enum": [4096, 8192, 16384, 32768, 65536] },
        "regions": {
          "type": "array", "minItems": 1, "maxItems": 1024,
          "items": { "$ref": "#/$defs/Region" }
        }
      }
    },
    "entries": { "type": "array", "items": { "$ref": "#/$defs/Entry" } }
  }
}
```

特别注意 ID 的 **JSON 形式是 32 个 0–255 整数的数组**，不是 hex string。文件名才是 hex。额外语义约束：

- `frame` 与 `fill` 恰有一个非 null；fill 表示整块重复同一字节。
- entries 的 block 升序且唯一；frame 顺序连续且全部 frame 必须被引用。
- base 的 parent=null，并覆盖所有块；delta 只覆盖部分块，parent 指向同目录祖先。
- layout region 有序、不重叠、总大小有界；parent 与 child layout 一致，链不循环。
- 解码长度与块长度一致，最后块可短；每个块核对 checksum。
- 当前 FUSE adapter 写入的 layout 是一个从 0 开始的紧凑文件 region，`page_bytes=4096`；这里的 guest_address=0 不能解释为原 VM 真实物理 region 只有一个。

### 其他文件

![原始 RAM、staging、pin、逻辑文件与 alias 的实际内容](assets/raw-and-staging-layout.svg)

| 文件 | 内部布局 | 是否可独立恢复 RAM |
|---|---|---|
| 原始 `vm.ram` | 紧凑 RAM 字节，无 pVisor header/schema；按 region offset 映射 | 只含 RAM，缺 CPU/设备状态 |
| staging `.tmp*` | 逻辑 offset 的原始有效页＋稀疏洞；掩码仅在内存 | 否，缺脏页掩码/epoch 和完整旧 chain |
| capture `.tmp*` | 尚在写的 `.pvdelta`；footer 未完成时没有完整格式 | 否，不能把未发布临时文件当 committed generation |
| `<id>.pvpin` | 0 字节；文件名本身就是 root ID | 否，它只是保留标志，需对应 generation chain |
| FUSE `mount-*/ram` | 合成的逻辑地址空间；read 动态解码/叠加 staging | 不持有独立物理 payload，也不是持久文件入口 |

## 4. 实验数据支撑 {#experiments}

2026-10-03 **只读检查已有产物**：`target/offload-validation/final-diagnostics/workdirs/semspec-S-DOC-062-8Fsw80/`。不是本轮新 VM 测试，也不作为当前实验工作树的通过证据。样本 committed layout 为 256 MiB；只剩一个 base＋一个 delta，未见 pin / 活动 staging。

```text
semspec-S-DOC-062-8Fsw80/
├── startup.ram                  999 B 逻辑；4096 B allocated
└── startup.ram.layers/
    ├── a8c9862d…f8b08ab.pvdelta  22486063 B 逻辑；22487040 B allocated；base
    └── 2304f79b…35bd9e4.pvdelta   1937862 B 逻辑； 1941504 B allocated；delta
```

| 文件 | 实测拆分 | 逻辑大小核对 |
|---|---|---:|
| manifest | N=155，k=10 | `44+155+80×10 = 999 B` |
| base | P=21797445，seek=10897（F=1360），J=677657，footer=64；4096 entries | `21797445+10897+677657+64 = 22486063 B` |
| delta | P=1901198，seek=1689（F=209），J=34911，footer=64；209 entries | `1901198+1689+34911+64 = 1937862 B` |

base 的 4096 entries 中只有 1360 个 frame，其余以 fill 表示，说明 entry 数≠frame 数。两个 generation 的 allocated 总和为 24428544 B（约 23.30 MiB）；还应另计 manifest、目录占用等，不能直接把它作为整 VM 内存占用或通用压缩率。

sample manifest 的 descriptor 指向原样本 layers **绝对路径**；迁走该目录而不重写格式会破坏引用。它有 10 条历史 head，不代表磁盘仍保留 10 份 generation。

## 5. 使用建议 {#usage}

容量统计使用 `st_blocks × 512`，并按 inode 去重；`st_size` 用于解析文件边界，不能代表稀疏文件或压缩 RAM 的物理占用。FUSE 逻辑文件报告的 blocks 也不能用于计算压缩后的磁盘成本。

保留或检查 compressed backing 时，应先停止 writer，再同时保留 manifest 与它引用的 layers 目录。目录路径写在 descriptor 中，复制 manifest、改名 alias 都不会调整这个路径。不要手动清理仍被 current head 或 pin 引用的 generation。

合并需要新旧 generation 同时存在；按完整 base、旧链、staging 和 pin 保留的数据预留空间。撕裂的 manifest 尾部目前会导致 open 失败，文件格式没有提供自动回退；归档前应确认最后一次 offload 已成功返回。
