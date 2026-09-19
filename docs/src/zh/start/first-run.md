# 第一次运行

这个示例在暂存项目中创建一个文件，审查后再应用到原项目，不需要 Agent 账号或模型 API。请先完成[安装](installation.md)，包括宿主机暂存所需的 FUSE/macFUSE。

## 1. 创建演示项目

```bash
mkdir -p pvisor-demo/project
cd pvisor-demo/project
printf 'original\n' > original.txt
```

## 2. 在暂存工作区运行命令

```bash
pvisor run --stage ../stage-001 -- /bin/sh -c 'printf "hello from the stage\n" > hello.txt'
```

命令的工作目录使用暂存视图。暂存目录放在项目外，避免运行元数据混入项目树。下一次运行应使用新的暂存目录。

```bash
test ! -e hello.txt
pvisor review last
```

此时原项目中没有 `hello.txt`，但审查结果中可以看到它。除了文件列表，也要阅读实际隔离能力和警告。如果暂存挂载失败，应先解决平台配置问题。

## 3. 接受改动

```bash
pvisor apply last --path hello.txt
cat hello.txt
```

原项目现在包含内容为 `hello from the stage` 的文件。如果希望拒绝尚未应用的改动，使用 `pvisor drop last`。Drop 不会撤销已经应用的文件。

## 4. 换成你的命令

在真实项目中，把示例命令换成你的脚本或自动化命令。已安装的 Agent CLI 也使用相同入口：

```bash
pvisor run --stage ../agent-stage-001 -- codex
pvisor review last
```

审查后，再选择 `pvisor apply last --path PATH`、`--all` 或 `pvisor drop last`。同时处理多个项目或运行时，建议用命令输出的明确 Run ID 或暂存路径代替 `last`。

继续阅读[审查与应用](../guides/review-apply.md)，了解分批处理、冲突和检查点。
