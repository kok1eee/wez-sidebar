# wez-sidebar

[Claude Code](https://docs.anthropic.com/en/docs/claude-code) のセッション・使用量・タスクを監視する WezTerm サイドバー / ドック。

[English](README.md)

| Sidebar (MacBook) | Dock (外部モニター) |
|:---:|:---:|
| ![Sidebar](docs/images/sidebar-with-panes.png) | ![Dock](docs/images/dock-mode.png) |

| モード選択 | Overlay | Sidebar 単体 |
|:---:|:---:|:---:|
| ![Select](docs/images/mode-select.png) | ![Overlay](docs/images/wezterm-overlay.png) | ![Sidebar](docs/images/sidebar-mode.png) |

## 機能

- **セッション監視** - Claude Code セッションの状態（実行中 / 入力待ち / 停止）、稼働時間、タスク進捗をリアルタイム表示
- **API 使用量** - Anthropic API の使用量（5時間制限・週間制限）をカラーコード付きで常時表示
- **タスクパネル** - 外部 JSON キャッシュファイル（Asana 等）からタスク一覧を表示（オプション）
- **内蔵 hook ハンドラー** - `sessions.json` を自律管理。外部依存なしで動作
- **2つの表示モード** - Sidebar（MacBook 向け右バー）または Dock（外部モニター向け下部バー）
- **ペイン連携** - Enter キーまたは数字キーで対象セッションの WezTerm ペインに即切り替え

## 必要環境

- [WezTerm](https://wezfurlong.org/wezterm/)
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code)
- Rust ツールチェーン（ビルド用）

## インストール

```bash
cargo install --path .
```

## セットアップ

### 1. Hook の登録

`~/.claude/settings.json` に以下を追加:

```json
{
  "hooks": {
    "PreToolUse": [
      { "type": "command", "command": "wez-sidebar hook PreToolUse" }
    ],
    "PostToolUse": [
      { "type": "command", "command": "wez-sidebar hook PostToolUse" }
    ],
    "Notification": [
      { "type": "command", "command": "wez-sidebar hook Notification" }
    ],
    "Stop": [
      { "type": "command", "command": "wez-sidebar hook Stop" }
    ],
    "UserPromptSubmit": [
      { "type": "command", "command": "wez-sidebar hook UserPromptSubmit" }
    ]
  }
}
```

### 2. WezTerm の設定

`wez-sidebar`（または `wez-sidebar dock`）を実行するサイドバー/ドックペインを追加。

右サイドバーとして起動するキーバインド例:

```lua
{
  key = "b",
  mods = "LEADER",
  action = wezterm.action_callback(function(window, pane)
    local tab = window:active_tab()
    -- 右にwez-sidebarを分割
    tab:active_pane():split({ direction = "Right", args = { "wez-sidebar" } })
  end),
}
```

### 3.（オプション）設定ファイルの作成

```bash
cp config.example.toml ~/.config/wez-sidebar/config.toml
```

利用可能な全オプションは [`config.example.toml`](config.example.toml) を参照。

## 設定項目

| キー | デフォルト | 説明 |
|------|-----------|------|
| `wezterm_path` | 自動検出 | WezTerm バイナリのパス |
| `stale_threshold_mins` | `30` | セッションを非アクティブと見なすまでの分数 |
| `data_dir` | `~/.config/wez-sidebar` | `sessions.json` の保存ディレクトリ |
| `hook_command` | *（内蔵）* | hook 処理を委譲する外部コマンド |
| `tasks_file` | *（なし）* | タスクキャッシュ JSON ファイルのパス |
| `task_filter_name` | *（なし）* | 担当者名でタスクをフィルタ |

## 使い方

### Sidebar モード（デフォルト）

```bash
wez-sidebar
```

### Dock モード（横長下部バー）

```bash
wez-sidebar dock
```

### キーバインド

| キー | Sidebar | Dock |
|------|---------|------|
| `j`/`k` | 上下移動 | 上下移動 |
| `Enter` | ペイン切り替え | ペイン切り替え |
| `t` | タスクモード | - |
| `Tab`/`h`/`l` | - | カラム移動 |
| `p` | プレビュー切替 | - |
| `f` | 全セッション表示切替 | 全セッション表示切替 |
| `d` | セッション削除 | セッション削除 |
| `r` | 全更新 | 全更新 |
| `?` | ヘルプ | ヘルプ |
| `q`/`Esc` | 終了 | 終了 |

## ライセンス

MIT
