# 先在沙盘里修，再带回项目

工时周报把 `1.5 + 2.0` 算成了 `3.0` 小时。下面逐步修复它：
先在沙盘里改，查看结果，再把修复带回原项目。

需要已安装 pVisor、系统 Python 3，以及可用的 safe Host 环境
（Linux FUSE/user namespaces 等，见[安装指南](../../../docs/src/zh/start/installation.md)）。
以下步骤已在 Linux 验证，无需模型账号或网络。这里直接用 Python 修改文件，
代替 Agent 的编辑操作；每一条 pVisor 命令都由你执行。

## 1. 准备一个小项目

从仓库根目录开始，在同一个终端逐段执行。
复制示例到一个新目录，避免改动模板：

```bash
PYTHON=/usr/bin/python3
cd examples/pvisor/08-stagebox-contract
mkdir -p .work
demo="$(mktemp -d "$PWD/.work/demo.XXXXXX")"
mkdir "$demo/project" "$demo/home"
cp weekly_report.py hours.csv test_report.py "$demo/project/"
cd "$demo/project"
```

查看输入并运行工具：

```bash
cat hours.csv
"$PYTHON" -B weekly_report.py
"$PYTHON" -B -m unittest discover -v
```

输入是周一 `1.5` 小时、周二 `2.0` 小时，工具却输出：

```text
本周总工时：3.0 小时
```

两项测试中 `test_fractional_hours` 失败。这是要修复的问题，
测试命令此时返回非零是预期行为。

## 2. 在沙盘里修改

下面的命令移除错误的整数截断，并顺手写一份临时调查记录。
所有修改先留在沙盘里。只给这条命令使用空的演示 HOME，不改变当前终端的 HOME：

```bash
HOME="$demo/home" pvisor spawn --no-agent-defaults --safe --overlaynet-deny-all \
  --stage "$demo/stage" -- "$PYTHON" -B -c '
from pathlib import Path
p = Path("weekly_report.py")
p.write_text(p.read_text().replace("return int(sum(hours))", "return sum(hours)"))
Path("scratch.txt").write_text("Temporary investigation notes\n")
'
```

进入沙盘的只读视图，运行工具和测试：

```bash
pvisor inspect "$demo/stage" -- "$PYTHON" -B weekly_report.py
pvisor inspect "$demo/stage" -- "$PYTHON" -B -m unittest discover -v
```

现在看到 `本周总工时：3.5 小时`，两项测试全部通过。

## 3. 看看原项目变了吗

不用 inspect，直接在当前目录运行：

```bash
"$PYTHON" -B weekly_report.py
ls
```

仍然输出 `本周总工时：3.0 小时`，目录里也没有 `scratch.txt`。
沙盘里的修复已经生效，原项目尚未改变。

## 4. 审查，选择要带回的文件

```bash
pvisor review "$demo/stage" --diff
```

在项目变化与 diff 中找到两项：

- `weekly_report.py`：`return int(sum(hours))` 改成 `return sum(hours)`。
- `scratch.txt`：新增临时调查记录。

只预览代码修复，不选择调查记录：

```bash
pvisor apply "$demo/stage" --path weekly_report.py --dry-run --diff
"$PYTHON" -B weekly_report.py
```

预览中只选择了 `weekly_report.py`。原项目仍输出 `3.0` 小时，
因为 dry-run 不会写入。

## 5. 带回修复，在原项目验证

确认修改符合预期后，执行：

```bash
pvisor apply "$demo/stage" --path weekly_report.py
"$PYTHON" -B weekly_report.py
"$PYTHON" -B -m unittest discover -v
```

这次原项目输出 `本周总工时：3.5 小时`，两项测试全部通过。

如果不想接受修复，可以跳过这一步，直接执行下一步丢弃全部候选。

## 6. 丢弃剩余调查记录

```bash
pvisor drop "$demo/stage"
ls
"$PYTHON" -B weekly_report.py
```

`scratch.txt` 没有进入原项目；如果你执行了上一步，已带回的修复仍在，
总工时仍为 `3.5` 小时。

项目保留在 `$demo/project`，你可以继续检查。再次体验时，从仓库根目录重新开始，
使用新的演示目录。

`apply --path` 会直接写入所选改动，执行前先审查，接纳期间停止其他写入者。
沙盘不是冻结快照或原子事务；drop 只丢弃未接纳的候选，不撤销已带回文件或外部副作用。
