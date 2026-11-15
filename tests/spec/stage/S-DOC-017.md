# S-DOC-017：C06 `--safe` 隔离 HOME 写入


准备：Linux user/mount namespace 可用。

**语义**：Agent 能在自己的 HOME 中读回刚写入的状态；宿主 HOME 没有该文件，workspace 的持久 stage 仍可审查。

**理由**：检查 `--safe` 除暂存 workspace 外，还为 HOME 提供独立的写时复制视图。示例把测试 HOME 放在专用目录，不触碰真实用户目录。

**违反示例**：--safe 把 HOME/state 写到了宿主 HOME，或 Agent 不能读取自己的写入。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-017 --timeout 180 <<'CASE'
> require_python3
> require_rootless
> case_setup
> case_run success <<'CASE_COMMAND'
> mkdir -p "$CASE_ROOT/home"
> HOME="$CASE_ROOT/home" pvisor --safe --stage "$CASE_ROOT/safe-home" -- \
>   /bin/sh -c 'printf private > "$HOME/state"; cat "$HOME/state"'
> CASE_COMMAND
>
> stdout_has private
> test ! -e "$PVISOR_CASE_ROOT/home/state"
> bundle_expect filesystem.state staged "$PVISOR_CASE_ROOT/safe-home"
> bundle_expect network.policy.mode allowlist "$PVISOR_CASE_ROOT/safe-home"
> CASE
PASS S-DOC-017
```
