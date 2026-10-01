# 暂存与 apply 语义

暂存把工作区改动留在 Overlay 中，显式审查和 apply 再改变目标。每条语义对应 semspec 用例（S-STAGE-xxx）：

- apply 前工作区逐项不变；
- 外部修改、新建、删除冲突时拒绝 apply，并保留外部内容；
- `--all` 遇到冲突时整体拒绝；
- drop 之后不能再 apply；重复 apply 报告「已应用」；
- 重命名显示为删除加新增；
- 不可逆的部分：外部 API、数据库、已发出的消息。

!!! note "TODO"
    逐条对应 S-STAGE-xxx 用例编号并给出可复现命令。
    与 guides/review-apply 去重。

