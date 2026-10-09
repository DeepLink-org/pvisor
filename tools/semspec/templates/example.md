# Example semantic specification (draft — requires human review)

### S-EXAMPLE-001: A successful invocation preserves an existing workspace

**语义**：调用公开入口并成功退出，不改变事先创建的工作区状态。

**理由**：成功不应隐含未声明的文件系统修改。

**违反示例**：命令退出 0，但删除了调用方已有的文件。

<!-- semspec: case id=S-EXAMPLE-001 -->
```bash
printf original > keep
snapshot before .
expect_exit 0 "${SUBJECT_BIN:-/usr/bin/true}"
assert_unchanged before .
```
