# OpenCode

!!! note "实验性"
    下面的命令只使用已有 pVisor 参数，但尚未完成固定 Agent 版本的端到端回归。

## 基于现有 CLI 的接入起点

先在所选 executor 内安装 OpenCode，并确认在独立测试项目中可以运行。下面以 api.openai.com:443 作为目标；示例假设你已在 Agent 自身配置里选择对应的 OpenAI 服务，pVisor 不替你配置 provider。

```bash
pvisor run --safe --overlaynet-allow api.openai.com:443 \
  --pass-env OPENAI_API_KEY -- opencode
pvisor status --review last
pvisor apply last --path src
```

显式 allow 会替换 `--safe` 预设列表，登录、其他 provider 或依赖下载需要逐个补充目标。只有直接可执行文件名才匹配 `--safe` 适配，shell 包装会改变匹配结果。`--safe` 下的 HOME 状态写入在退出后丢弃，不要依赖本次运行保存登录状态。
