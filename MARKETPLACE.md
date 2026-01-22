# CC-Workbench Marketplace

通过 Claude Code Plugin Marketplace 安装 cc-workbench。

## 安装方法

### 1. 添加插件市场

在 Claude Code 中运行：

```
/plugin marketplace add https://github.com/Siiichenggg/cc-workbench
```

### 2. 安装插件

```
/plugin install cc-workbench
```

### 3. 运行安装命令

```
/install-cc-workbench
```

或者直接运行：

```
/install
```

## 功能特性

- **TUI 工作台**：分屏显示 Claude CLI 和工作台
- **用量监控**：实时显示 token 使用情况和限额
- **上下文追踪**：展示当前上下文使用量
- **Git 快照**：自动创建 Git patch 快照
- **快照恢复**：一键恢复到历史快照
- **历史记录**：查看和跳转到历史消息

## 快捷键

- `Ctrl+Q`：退出
- `Tab`：切换焦点到历史面板
- `Enter`：跳转到选中消息
- `d`：查看 diff 预览
- `r`：恢复快照
- `PageUp/PageDown`：滚动输出
- `End`：跟随输出

## 配置

在工作区或主目录创建 `.cc-workbench/config.json`：

```json
{
  "context_limit": 200000,
  "compress_threshold": 0.85,
  "usage_poll_seconds": 30,
  "providers": [
    {"type": "local", "name": "local-estimate", "limit_tokens": 200000}
  ]
}
```

## 卸载

运行卸载命令或手动恢复：

```bash
# 找到 claude 目录
which claude

# 恢复原始 claude
mv $(dirname $(which claude))/claude.real $(which claude)
```

## 更多信息

完整文档请参考项目主页。
