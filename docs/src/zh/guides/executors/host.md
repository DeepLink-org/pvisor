# host 执行器

Linux 使用 rootless launcher、user/mount/PID namespace、投影根目录与协商后的 Landlock；macOS 使用 Seatbelt。默认 host 文件系统视图不受限制；`--filesystem sandbox` 启用文件系统控制，`--overlaynet-deny-all` 独立启用网络边界。

!!! note "TODO"
    补前提、控制范围、已知缺口与 Run Bundle 中的证据字段。

