---
status: todo
search:
  exclude: true
---

# aider

!!! warning "规划中"
    本页尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

如何在 pVisor 边界内运行 aider 并控制其网络与文件访问？

## 需求

- 指标：接入步骤可复现、注入方式正确、实际控制有证据
- 对照组：Agent 自带沙箱或审批模式
- 工作负载：一次代表性任务（读取、写入、访问模型 API）
- 环境：固定 Agent 版本；平台相关需注明

## 验收标准

- 给出可复制的接入命令与验证步骤
- 写明注入方式与已知限制
- 受支持版本固定并有回归

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：guides/agents/index、reference/platforms

## 基于现有 CLI 的接入起点

先在所选 executor 内安装 aider，并确认在独立测试项目中可以运行。下面以 api.openai.com:443 作为目标；aider/OpenCode 示例假设你已经在 Agent 自身配置里选择对应 OpenAI 服务，并非替你配置 provider。

```bash
pvisor run --safe --overlaynet-allow api.openai.com:443 \
  --pass-env OPENAI_API_KEY -- aider
pvisor status --review last
pvisor apply last --path src
```

显式 allow 会替换 safe 预设列表，登录、其他 provider 或依赖下载需要逐个补充目标。直接可执行文件名才匹配 safe 适配，shell 包装会改变匹配结果。safe HOME 的状态写入退出后丢弃，所以不要依赖本次运行保存下次登录状态。

这组命令只使用已有 pVisor 参数；本页尚未完成固定 Agent 版本的端到端回归。先用[第一次运行](../../start/first-run.md)核对暂存、拒绝与选择性 apply，再用 Bundle 核对真实 Agent。Linux host 的选择性代理是协作式；需要强制边界时使用已安装 Agent 的 VM。
