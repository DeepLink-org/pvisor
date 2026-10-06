# KSM 开启后的四 VM 去重效果预检

## 主要结论

**KSM 在 pVisor 恢复实例的私有 RAM 上确实发生了合并。20 秒观察窗周围，重复页与跨 VM 相同随机页的四 VM RAM-PSS 分别减少约 157 MiB、138 MiB；相应建议 off 条件没有类似变化，实例独有随机页的 RAM-PSS 基本不变。** 六格均通过完整性、COW 隔离及退出检查。

这是 B-MEMORY-SCALE engineering A/B 的独立扫描开启预检 cohort，单格一个新组，没有正式多轮统计。数字不是最终收敛量、生产容量、跨批因果比例或整机净节省。

## 实验设定

- Linux `7.2.8-200.fc44.x86_64`；管理员已启用 KSM。脚本不修改任何全局 KSM 配置。
- 捕获的全部快照保持 `run=1`、`pages_to_scan=100`、`sleep_millisecs=20`；仅能证明采样时一致，不能证明采样间无变化。
- 六格：四 VM × `repeated/random-shared/random-unique` × advice off/on；实际同时 VM 上限为 4。Advice off 时宿主 scanner 仍开启。
- 每 VM 256 MiB 配置 RAM、1 vCPU、64 MiB payload；整体四核 quota、2 GiB 上限、零 swap，500 ms settle。
- 每格新的生产者捕获 raw checkpoint，先 suspend/reap 再恢复四个实例；显式无网络。
- ready 保留共同不可变基线；dynamic duplicate 写遍全部 payload，使其成为私有 COW 页；固定 20 秒观察窗、全摘要读回和停驻 barrier，然后 25/100% 实例独有 mutation、单实例取消、幸存者读回及有序退出。
- 初始窗口与动态窗口各 20 秒。命名动态 barrier 在窗口前后另包含 resume、读回和 pause 工作，所以其差值不是恰好 20 秒隔离扫描活动。
- 只从实际 runner 的 FD200 backing device/inode 匹配的三段 `rw-p` RAM VMA 累加 PSS/KSM/Private_Dirty；不使用 whole-process 汇总代替 RAM 指标。
- 每 runner 对应 297,533,440 bytes RAM VMA；共同 inode 在各四 VM 组内核实。不同组的 mount 身份重复编号不证明跨组共享。
- Advice on 的全部匹配 VMA 有 `mg`，off 没有；登记标记与实际 smaps KSM 分开记录。

## 实测结果

### RAM-PSS 与实际 KSM

单位 MiB，四 VM 匹配 RAM VMA 合计；before/after 为 `dynamic_private_before_wait` / `dynamic_private_after_wait`。

| 内容 | Advice | RAM-PSS before → after | PSS 变化 | 实际 RAM-KSM before → after |
| --- | --- | ---: | ---: | ---: |
| 重复页 | off | 298.972 → 299.608 | +0.637 | 0 → 0 |
| 重复页 | on | 295.857 → 138.931 | −156.927 | 3.953 → 161.977 |
| 跨 VM 相同随机页 | off | 300.716 → 301.191 | +0.476 | 0 → 0 |
| 跨 VM 相同随机页 | on | 299.090 → 161.070 | −138.020 | 2.062 → 204.434 |
| 实例独有随机页 | off | 300.542 → 300.732 | +0.190 | 0 → 0 |
| 实例独有随机页 | on | 299.235 → 299.169 | −0.066 | 1.156 → 1.434 |

变化量从未舍入数值计算，因此可能与表中已舍入端点相减相差 0.001 MiB。

KSM 字段是这些映射中实际使用 KSM 页的字节数合计，**不是唯一物理 KSM 占用或节省字节**。同一共享页可计入多个映射。RAM VMA 包含 guest OS/runtime，缺少 guest payload 地址到宿主页的映射，不能声称所有 KSM 字节都来自 payload。

正对照的 RAM Private_Dirty 分别减少 157.566 MiB、202.059 MiB，与 PSS/KSM 变化共同支持重复私有页合并。负对照可能仍合并少量相同 OS/runtime 页，因此不要求全进程或 RAM KSM 恰为零。

### 完整 cgroup 与计费归因

单位 MiB；包含 runner、协调器、backing/cache 和内核计费，而非整机。

| 内容 | Advice | memory.current before → after | anon 变化 | file 变化 |
| --- | --- | ---: | ---: | ---: |
| 重复页 | off | 848.527 → 853.035 | +1.469 | +1.027 |
| 重复页 | on | 852.027 → 698.398 | −156.633 | +0.527 |
| 相同随机页 | off | 851.047 → 854.129 | +1.379 | +0.477 |
| 相同随机页 | on | 848.441 → 713.781 | −136.730 | +0.527 |
| 独有随机页 | off | 726.574 → 601.676 | +1.043 | −126.914 |
| 独有随机页 | on | 849.836 → 819.160 | +0.789 | −33.230 |

正对照建议 on 的组计费分别减少 153.629 MiB、134.660 MiB，主要对应 anon 减少。**独有随机页 off 的组计费也减少了 124.898 MiB，但 RAM-PSS 没降，变化来自 file 计费下降，不能叫去重收益。** 快照不能确定 file 计费变化的具体原因，证明了不能仅凭 memory.current 推断合并。

### 写入拆分与未收敛

| 内容，advice on | 动态窗口后 RAM-PSS / KSM | cow25 RAM-PSS / KSM | cow100 RAM-PSS / KSM |
| --- | ---: | ---: | ---: |
| 重复页 | 138.931 / 161.977 | 155.841 / 145.398 | 299.275 / 1.457 |
| 相同随机页 | 161.070 / 204.434 | 165.348 / 183.914 | 299.837 / 1.609 |

全量不同写入后恢复到约 299 MiB 私有 RAM-PSS，KSM 降到少量残留，与写入拆分共享的预期一致；全摘要和 peer-isolation 检查通过。Scanner 全程继续运行，所以这些阶段不是纯粹冻结扫描后的 COW 成本测量。

动态窗口后，四 runner 的 PSS 范围：重复页 on 为 10,443–76,685 KiB，相同随机页 on 为 32,536–65,883 KiB。进度不均匀；不能声称全部实例已充分或均匀合并。

## 正确性与成本边界

- 六格计划/尝试/通过均为 6，失败/未执行均为 0；每格 74 项检查，合计 444/444 通过。
- 全部 payload 摘要、mutable scratch、恢复 heartbeat、私有 COW offload 拒绝、peer 写入隔离、取消/reap、幸存者摘要和有序退出通过。
- 全部 cleanup `all_reaped=true`，owned unit quiescent，无捕获的 OOM/max 事件或进程/FD 读取错误。
- 每格 elapsed 为 108.656–111.966 秒，不是扫描耗时。
- group CPU 包含 guest 工作、摘要、barrier 与采样，**不包含宿主 ksmd 的 CPU**；没有扫描器 CPU 效率结果。
- 全局 full_scans 的动态窗口在 advice-on 重复/相同随机/独有随机格分别增加 1/1/2；它是宿主上下文，不等于本组 payload 扫描覆盖。
- 六格退出后宿主 pages_shared/pages_sharing 回到 0，与实例退出、共享页释放相容，不代表运行期间未合并；运行中实际 smaps 证据已保留。

## 实际命令与来源

从当前 `pvisor` 仓库根执行的命令：

```sh
python3 benchmark/pvisor/memory_scale.py \
  --example target/debug/examples/vm_memory_scale \
  --build-receipt benchmark/pvisor/.data/memory-scale-build5-20261006/build-receipt.json \
  --rootfs /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/density-env4/rootfs \
  --firmware /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/firmware \
  --output /home/reiase/workspace/pvisor/benchmark/.data/s4k \
  --concurrencies 4 --modes ksm \
  --patterns repeated,random-shared,random-unique --dedup off,on \
  --ksm-wait-seconds 20 --preflight
```

完整原始数据、逐 VMA smaps、checks、命令、输入/源码/制品 receipts 留在本地 `/home/reiase/workspace/pvisor/benchmark/.data/s4k/`。按规则 `.data/` 不进入 Git，复测必须使用新的短输出路径。

| 制品 | SHA-256 |
| --- | --- |
| frozen binary | `56311da9e4df691c7145597efa6b297e47a0dd3f1098e8e0cfc5e771e747a048` |
| build-time source manifest | `bbd923a30b9f920b44e2eea3cf3f592863dbae1392649bccf4c62f54784091ff` |
| frozen coordinator | `bea6fe02c506dc21d60c00ba55135049da288f783fc18b211c4058840c5d959e` |
| build receipt | `a1a76509edf6e9416c0d846815d711049806926b38939ee2dff692fb5ae6ef97` |

HEAD 为 `99e086f50568e135b40c3d717afd2f932890747b` 加实验工具未提交源码；当前源码与构建源码清单分开保留。构建收据的摘要匹配已复核，但来源断言不是独立重建证明，外部 registry 源码未冻结。

## 下一步

当前已回答“开启 KSM 后是否真实合并、正负内容对照是否合理”。尚未回答稳定态/最大收益、1/2/4 VM 动态随机页扩展曲线、scanner CPU、业务延迟和生产密度。进一步测量应事先固定更长观察窗与匹配 deadline，单列 cohort；增加独立重复组后再做配对不确定性分析，不能把六格预检的单样本差异当正式统计。
