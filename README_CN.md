# cc-workbench

本地 Claude Code CLI 的侧边工作台：自动分屏 TUI，展示用量/上下文/历史，并支持 Git patch 快照与恢复。

## 安装

### 方式 1：通过 Claude Code Marketplace（推荐）

```
/plugin marketplace add https://github.com/Siiichenggg/cc-workbench
/plugin install cc-workbench
/install
```

### 方式 2：手动构建

## 构建

```
cargo build --release
```

产物：`target/release/cc-workbench`

## 安装（包装器）

该工具会包装真实 Claude CLI。启动后左侧是 Claude 输出，右侧是工作台。

### 方式 A：手动 alias

```
alias claude="/path/to/target/release/cc-workbench"
```

### 方式 B：重命名 + shim

1) 找到当前 Claude 二进制：

```
which claude
```

2) 重命名为 `claude.real`（同目录）。

3) 将 `cc-workbench` 放在同目录并命名为 `claude`。

如果包装器旁边存在 `claude.real`，会自动调用它；也可手动指定：

```
export CCWB_CLAUDE_CMD=claude.real
```

### 方式 C：脚本安装

```
./scripts/install.sh /path/to/target/release/cc-workbench
```

## 使用

像平时一样运行 `claude`，右侧工作台会自动出现。

### 快捷键

- `Ctrl+Q`：退出
- `Tab`：聚焦历史面板
- `Enter`（历史面板）：跳转到对应输出位置
- `d`（历史面板）：查看 diff 预览
- `r`（历史面板）：diff 预览 + 恢复确认
- `y`/`n`（diff 预览）：确认/取消恢复
- `PageUp`/`PageDown`：滚动输出
- `End`：回到底部并跟随输出

## 配置

在工作区创建 `.cc-workbench/config.json`（或 `~/.cc-workbench/config.json`）配置上下文与用量 provider。

示例：

```
{
  "context_limit": 200000,
  "compress_threshold": 0.85,
  "usage_poll_seconds": 30,
  "providers": [
    {"type": "local", "name": "local-estimate", "limit_tokens": 200000},
    {"type": "manual", "name": "claude", "limit_tokens": 1000000, "used_tokens": 12345},
    {
      "type": "httpjson",
      "name": "glm",
      "url": "https://api.example.com/usage",
      "method": "GET",
      "headers": {"Authorization": "Bearer YOUR_KEY"},
      "used_pointer": "/data/used",
      "limit_pointer": "/data/limit"
    }
  ]
}
```

## 数据目录

每个工作区的数据保存在 `.cc-workbench/`：

- `ccwb.sqlite`：会话元数据
- `snapshots.git`：Git patch 快照历史
- `backup/`：恢复前备份

## 说明

- 默认用量展示为本地 token 估算。
- 快照系统会排除 `.cc-workbench`。
- `httpjson` 使用 JSON Pointer（RFC 6901），如 `/data/usage/used`。
- `httpjson` 内部使用系统 `curl`（macOS 默认自带）。

## Provider 模板

拿到官方 usage 接口后即可配置 Claude/GLM/Minimax：

```
{
  "type": "httpjson",
  "name": "claude",
  "url": "https://<official-usage-endpoint>",
  "method": "GET",
  "headers": {"Authorization": "Bearer <KEY>"},
  "used_pointer": "/usage/used",
  "limit_pointer": "/usage/limit"
}
```
