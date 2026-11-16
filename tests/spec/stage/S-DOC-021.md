# S-DOC-021：D04 显式拒绝敏感路径读取


准备：Linux user/mount namespace 可用。

**语义**：读取失败并记录拒绝规则；宿主文件保持原样。

**理由**：用 `--access PATH-GLOB:deny` 阻止 Agent 在工作区中读取匹配的文件。

**违反示例**：private/token 被允许读取，或拒绝后审查记录没有该目标。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-021 --timeout 180 <<'CASE'
> require_python3
> require_rootless
> case_setup
> case_run nonzero <<'CASE_COMMAND'
> mkdir -p private
> printf secret > private/token
> pvisor --stage "$CASE_ROOT/access-stage" --access 'private/**:deny' -- \
>   /bin/cat private/token
> CASE_COMMAND
>
> stdout_has 'pVisor file access denied'
> bundle_expect filesystem.access_policy.deny.0 'private/**'
> bundle_contains run_observation.filesystem.paths private/token
> test "$(cat private/token)" = secret
> CASE
PASS S-DOC-021
```
