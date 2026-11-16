# S-DOC-018：D01 高级 OverlayFS 组合


建议场景：适合检查 OverlayFS 视图和 host 安全边界。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：记录的目标为 `view`，从顶层到底层依次为 `layer`、`base`、本次运行持有的 workspace 快照。目录为空，因此此例检查配置顺序，不检查同名文件覆盖内容。

**理由**：把宿主的两个目录依次叠加到工作区视图，并指定 Agent 看到的路径。`directory` 选择目录后端；改动只通过显式 `apply` 提交。

**违反示例**：记录中 lower 的 layer/base 顺序颠倒，或 workspace 快照目录缺失。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-018 --timeout 180 <<'CASE'
> require_python3
> require_stage
> case_setup
> case_run success <<'CASE_COMMAND'
> mkdir -p "$CASE_ROOT/base" "$CASE_ROOT/layer" "$PWD/view"
> pvisor \
>   --stage "$CASE_ROOT/composed-stage" \
>   --mount "$CASE_ROOT/base:$PWD/view:stage" \
>   --mount "$CASE_ROOT/layer:$PWD/view:stage" \
>   -- "$CASE_TRUE"
> CASE_COMMAND
>
> bundle_expect filesystem.state staged
> record_expect overlay_lowers.0 "$(realpath "$PVISOR_CASE_ROOT/layer")"
> record_expect overlay_lowers.1 "$(realpath "$PVISOR_CASE_ROOT/base")"
> record_contains overlay_lowers.2 "$PVISOR_CASE_ROOT/composed-stage/.overlay-lowers/"
> test -d "$(record_get overlay_lowers.2)"
> CASE
PASS S-DOC-018
```
