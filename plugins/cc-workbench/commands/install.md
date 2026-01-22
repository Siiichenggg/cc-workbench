---
description: "安装 cc-workbench 到本地系统"
allowed-tools: "Bash, Read"
---

# 安装 cc-workbench

立即执行以下步骤安装 cc-workbench：

## Step 1: 检查系统环境

检查是否已安装 Rust 工具链：

```bash
rustc --version 2>/dev/null || echo "Rust not installed"
cargo --version 2>/dev/null || echo "Cargo not installed"
```

检查是否已安装 claude：

```bash
which claude
```

如果 Rust 未安装，提示用户先安装 Rust：
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

## Step 2: 获取 cc-workbench 源码

如果当前不在 cc-workbench 目录中，克隆仓库：

```bash
# 检查当前目录是否有 Cargo.toml（cc-workbench 标志）
if [ ! -f "Cargo.toml" ]; then
  if [ -d "cc-workbench" ]; then
    cd cc-workbench
  else
    git clone https://github.com/Siiichenggg/cc-workbench.git
    cd cc-workbench
  fi
fi
```

## Step 3: 编译项目

```bash
cargo build --release
```

编译产物位于 `target/release/cc-workbench`

## Step 4: 安装 cc-workbench

获取当前 claude 二进制路径：

```bash
CLAUDE_PATH=$(which claude)
CLAUDE_DIR=$(dirname "$CLAUDE_PATH")
```

备份并替换 claude 二进制：

```bash
# 重命名原 claude 为 claude.real
if [ -f "$CLAUDE_PATH" ] && [ ! -f "$CLAUDE_DIR/claude.real" ]; then
  mv "$CLAUDE_PATH" "$CLAUDE_DIR/claude.real"
fi

# 复制 cc-workbench 到 claude 位置
cp target/release/cc-workbench "$CLAUDE_PATH"
chmod +x "$CLAUDE_PATH"
```

## Step 5: 验证安装

```bash
claude --version
```

显示安装成功信息，告知用户：
- cc-workbench 已安装
- 下次运行 `claude` 时会自动启动工作台
- 快捷键：Ctrl+Q 退出，Tab 切换焦点
- 配置文件位置：~/.cc-workbench/config.json

## Step 6: 显示卸载方法

如果需要卸载，执行：

```bash
# 恢复原始 claude
if [ -f "$CLAUDE_DIR/claude.real" ]; then
  rm "$CLAUDE_PATH"
  mv "$CLAUDE_DIR/claude.real" "$CLAUDE_PATH"
fi
```
