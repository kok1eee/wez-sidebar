# wez-sidebar

[Claude Code](https://docs.anthropic.com/en/docs/claude-code) のセッションをリアルタイム監視する WezTerm サイドバー / ドック。

[English](README.md)

## なぜ WezTerm か

WezTerm はペイン分割・セッション管理を内蔵しており、tmux を置き換える。wez-sidebar は **WezTerm のペインとして動作** し、WezTerm CLI（`wezterm cli list`, `wezterm cli get-text`, `wezterm cli activate-pane`）でセッション情報を取得する。

これは意図的なスコープ。WezTerm で複数の Claude Code セッションをペイン分割で並列実行しているユーザー向けのツール。他のターミナルでは動作しないが、それは制約ではなく特徴として割り切っている。

## 機能

- **セッション監視** — ステータス（実行中 / 入力待ち / 停止）、稼働時間、git ブランチ、リアルタイムアクティビティ
- **アクティビティ表示** — 各セッションが今何をしているか（`Edit src/config.rs`, `Bash cargo test` 等）
- **危険コマンド警告** — `rm -rf`, `git push --force` 等を赤字 + ⚠ マーカーで警告
- **ユーザーメッセージ表示** — 直近のユーザー発言 + 経過時間（`バグを直して (3m前)`）
- **タスク進捗（ドック）** — Claude の `TodoWrite` タスクをドックモードで表示（✓ 完了, ● 進行中, ○ 未着手）
- **yolo モード検出** — `--dangerously-skip-permissions` をプロセスツリー遡行で自動検出
- **API 使用量** — Anthropic API 使用量（5時間 / 週間）をカラーコード表示
- **2つの表示モード** — Sidebar（MacBook 向け右ペイン）/ Dock（外部モニター向け下部ペイン）
- **ペイン切り替え** — Enter キーまたは数字キーで対象セッションの WezTerm ペインに即ジャンプ
- **デスクトップ通知** — permission prompt 時に macOS 通知（`terminal-notifier` 使用）
- **ポーリングなし** — すべて hook → file watcher のプッシュ型。CPU 負荷ゼロ

## 必要環境

- [WezTerm](https://wezfurlong.org/wezterm/)
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code)
- Rust ツールチェーン（ソースビルド時のみ）

## インストール

### バイナリ（Rust 不要）

```bash
# macOS (Apple Silicon)
curl -L https://github.com/kok1eee/wez-sidebar/releases/latest/download/wez-sidebar-aarch64-apple-darwin \
  -o ~/.local/bin/wez-sidebar && chmod +x ~/.local/bin/wez-sidebar

# macOS (Intel)
curl -L https://github.com/kok1eee/wez-sidebar/releases/latest/download/wez-sidebar-x86_64-apple-darwin \
  -o ~/.local/bin/wez-sidebar && chmod +x ~/.local/bin/wez-sidebar

# Linux (x86_64)
curl -L https://github.com/kok1eee/wez-sidebar/releases/latest/download/wez-sidebar-x86_64-linux \
  -o ~/.local/bin/wez-sidebar && chmod +x ~/.local/bin/wez-sidebar
```

### Cargo

```bash
cargo install wez-sidebar
```

### ソースから

```bash
git clone https://github.com/kok1eee/wez-sidebar.git
cd wez-sidebar
cargo install --path .
```

## クイックスタート

セットアップウィザードを実行:

```bash
wez-sidebar init
```

以下を対話的にセットアップ:
1. Claude Code hooks を `~/.claude/settings.json` に登録
2. WezTerm キーバインドの案内

<details>
<summary>手動セットアップ</summary>

#### 1. Hook の登録

`~/.claude/settings.json` に追加:

```json
{
  "hooks": {
    "PreToolUse": [
      { "type": "command", "command": "~/.cargo/bin/wez-sidebar hook PreToolUse" }
    ],
    "PostToolUse": [
      { "type": "command", "command": "~/.cargo/bin/wez-sidebar hook PostToolUse" }
    ],
    "Notification": [
      { "type": "command", "command": "~/.cargo/bin/wez-sidebar hook Notification" }
    ],
    "Stop": [
      { "type": "command", "command": "~/.cargo/bin/wez-sidebar hook Stop" }
    ],
    "UserPromptSubmit": [
      { "type": "command", "command": "~/.cargo/bin/wez-sidebar hook UserPromptSubmit" }
    ]
  }
}
```

#### 2. WezTerm キーバインド

```lua
-- サイドバー（MacBook 向け）
{
  key = "b",
  mods = "LEADER",
  action = wezterm.action_callback(function(window, pane)
    pane:split({ direction = "Right", size = 0.2, args = { "wez-sidebar" } })
  end),
}

-- ドック（外部モニター向け）
{
  key = "d",
  mods = "LEADER",
  action = wezterm.action_callback(function(window, pane)
    pane:split({ direction = "Bottom", size = 0.25, args = { "wez-sidebar", "dock" } })
  end),
}
```

</details>

これだけで動く。設定ファイルは不要。

## カード表示

### サイドバー（コンパクト、コンテンツ 3 行）

```
╭─ 🟢 my-project ⠋ ────╮
│ 2h30m  main           │
│ Edit src/config.rs     │
│ バグを直して (3m前)    │
╰───────────────────────╯
```

### ドック（タスク進捗付き）

```
╭─ 🟢 my-project ⠋ ─────────────╮
│ 2h30m  main                    │
│ Edit src/hooks.rs              │
│ 認証機能を実装して (5m前)       │
│ ✓ 型を追加                     │
│ ● hooks を編集                 │
│ ○ テスト追加                   │
╰────────────────────────────────╯
```

同じ `render_session_card` 関数が `area.height` に応じて動的にコンテンツ量を調整する。モード分岐のコードは不要。

## セッションマーカー

| マーカー | 意味 |
|----------|------|
| 🟢 | 現在のペイン |
| 🔵 | 他のペイン |
| 🤖 | yolo モード（`--dangerously-skip-permissions`） |
| ⚫ | 切断済み |

| ステータス | 意味 |
|------------|------|
| ⠋ (spinner) | 実行中 |
| ? | 入力待ち（permission prompt） |
| ■ | 停止済み |

## 設定項目

すべてオプション。カスタマイズが必要な場合のみ `~/.config/wez-sidebar/config.toml` を作成。

| キー | デフォルト | 説明 |
|------|-----------|------|
| `wezterm_path` | 自動検出 | WezTerm バイナリのフルパス（PATH 問題がある場合に設定） |
| `stale_threshold_mins` | `30` | セッションを非アクティブと見なすまでの分数 |
| `data_dir` | `~/.config/wez-sidebar` | `sessions.json` / `usage-cache.json` の保存先 |

## キーバインド

| キー | Sidebar | Dock |
|------|---------|------|
| `j`/`k` | 上下移動 | 上下移動 |
| `Enter` | ペイン切り替え | ペイン切り替え |
| `1`-`9` | 番号で切り替え | 番号で切り替え |
| `Tab`/`h`/`l` | - | カラム移動 |
| `p` | プレビュー切替 | - |
| `f` | 全セッション表示切替 | 全セッション表示切替 |
| `d` | セッション削除 | セッション削除 |
| `r` | 全更新 | 全更新 |
| `?` | ヘルプ | ヘルプ |
| `q`/`Esc` | 終了 | 終了 |

## アーキテクチャ

```
Claude Code ──hook──→ wez-sidebar hook <event>
                              │
                    ┌─────────┴───────────┐
                    │ session 更新         │
                    │ activity 抽出        │
                    │ danger 検出          │
                    │ user message 取得    │
                    │ TodoWrite タスク取得 │
                    │ git branch 取得      │
                    │ yolo mode 検出       │
                    └─────────┬───────────┘
                              │
                    sessions.json + usage-cache.json
                              │
                         file watcher
                              │
                    wez-sidebar TUI（ポーリングなし）
```

すべてのデータは hook 経由で流れる。TUI はファイル変更にのみ反応し、ポーリングもサブプロセスも走らない。

## ライセンス

MIT
