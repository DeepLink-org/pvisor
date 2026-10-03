# 命令与场景参考

- [`pvisor` 命令参考](cli.md)：命令、参数、默认行为与配置模型。
- [完整环境快照 CLI](../design/environment-snapshot.md)：macOS 上的启动、保存、恢复与对象管理。
- [共享镜像缓存协议](shared-image-cache.md)：OCI 镜像文件缓存的客户端／服务端协议与远程访问。
- [可执行场景附录](cases.md)：按任务选择运行、暂存、权限、VM、容器、网络和回放场景。

首次使用从[第一个 Job](../start/first-run.md)开始。场景附录同时是 semspec 的 DOC 规格源，完整检查通过仓库中的 `just cases` 执行；检查成功与人工批准是两件事。

命令参考描述当前公共接口；概念见[概念与边界](../concepts/index.md)，机制与已知缺口见[实现设计](../design/index.md)。
