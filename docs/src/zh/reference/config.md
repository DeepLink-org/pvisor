# 配置文件参考（RunConfig）

`--config` 接受 TOML `RunConfig`，`--spec` 接受准备好的 JSON `RunSpec`。显式 scalar 选项覆盖文件值；重复的 list 选项替换整个列表；`--` 后的命令替换 `run.command`。

!!! note "TODO"
    补完整字段表：run、overlayfs、overlaynet、vm、container、gateway、policies、resource limits。
    与 reference/cli 去重：参数语法归 CLI 参考，文件字段归本页。

