# 任意脚本与自动化命令

pVisor 不要求 Agent；任何命令都可以在边界内运行：

```bash
pvisor run --safe --overlaynet-deny-all --stage ../stage-001 -- ./agent.sh
```

命令的工作目录使用暂存视图。文件与网络的实际控制以 Run Bundle 为准，而不是由参数推定。

!!! note "TODO"
    补充参数传递、环境变量投影、退出码与超时处理示例。

