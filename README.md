# cc-workbench

Claude Code workbench for local CLI: automatic side TUI with usage, context, history, and Git patch restore.

中文说明见 `README_CN.md`。

## Build

```
cargo build --release
```

Binary: `target/release/cc-workbench`

## Install (shim)

This tool wraps the real Claude CLI. The wrapper starts a TUI with the Claude process on the left and the workbench on the right.

### Option A: manual alias

```
alias claude="/path/to/target/release/cc-workbench"
```

### Option B: rename + shim

1) Locate your current Claude binary:

```
which claude
```

2) Rename it to `claude.real` (same directory).

3) Place `cc-workbench` in the same directory and name it `claude`.

The wrapper will call `claude.real` automatically if it sits next to the wrapper binary. You can also force a command:

```
export CCWB_CLAUDE_CMD=claude.real
```

### Option C: helper script

```
./scripts/install.sh /path/to/target/release/cc-workbench
```

## Usage

Run `claude` as usual. The workbench appears automatically on the right.

### Keys

- `Ctrl+Q`: quit
- `Tab`: focus history panel
- `Enter` (history): jump to message output
- `d` (history): diff preview
- `r` (history): diff preview + restore prompt
- `y`/`n` (diff): confirm/cancel restore
- `PageUp`/`PageDown`: scroll output
- `End`: follow output

## Config

Create `.cc-workbench/config.json` in your workspace (or `~/.cc-workbench/config.json`) to set limits and providers.

Example:

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

## Data

Per workspace data is stored in `.cc-workbench/`:

- `ccwb.sqlite` session metadata
- `snapshots.git` Git patch history
- `backup/` restore backups

## Notes

- Usage panel uses local token estimation by default.
- Snapshot system excludes `.cc-workbench`.
- `httpjson` providers accept JSON Pointer paths (RFC 6901). Example: `/data/usage/used`.
- `httpjson` providers use `curl` under the hood (macOS default).

## Provider templates

You can add Claude/GLM/Minimax usage once you have their official usage endpoints and JSON paths. Example:

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
