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
- **サブエージェント追跡** — アクティブなサブエージェント数を親セッションカードに表示
- **切断セッション表示** — WezTerm ペインが閉じられたセッションを ⚫ マーカーで表示（24 時間保持）
- **yolo モード検出** — `--dangerously-skip-permissions` をプロセスツリー遡行で自動検出
- **API 使用量** — Anthropic API 使用量（5時間 / 週間）をカラーコード表示
- **2つの表示モード** — Sidebar（MacBook 向け右ペイン）/ Dock（外部モニター向け下部ペイン）
- **ペイン切り替え** — Enter キーまたは数字キーで対象セッションの WezTerm ペインに即ジャンプ
- **デスクトップ通知** — permission prompt 時に macOS 通知（`terminal-notifier` 使用）
- **孤児プロセス自動クリーンアップ** — WezTerm ペインに紐づかない孤児 Claude Code プロセスを検出・kill（オプトイン）
- **ポーリングなし** — すべて hook → file watcher のプッシュ型。CPU 負荷ゼロ
- **新セッション spawn** — `wez-sidebar new <dir>` で別ディレクトリの Claude Code セッションを新タブに起動
- **カンバン / タスク管理** — タスクをバックログに積み、依存関係を張り、`Active / Review / Done` 列で進捗を俯瞰。承認で依存する次タスクを自動 spawn
- **ブロック検知通知** — `review` 状態が一定時間以上続くとデスクトップ通知 (放置防止)

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

## 新しいセッションを開く

`wez-sidebar new` で新しい WezTerm タブ（または新ウィンドウ）を開き、指定ディレクトリで `claude` を起動する。tmux バックエンドにも対応。

```bash
# カレントディレクトリで新タブ + claude
wez-sidebar new

# 指定ディレクトリで新タブ
wez-sidebar new ~/Documents/personal-dev/wez-sidebar

# 新しいウィンドウで開く
wez-sidebar new -w ~/Documents/personal-dev/wez-sidebar

# claude に初期プロンプトを渡す（`--` 以降は claude にパススルー）
wez-sidebar new ~/path/to/repo -- "src/foo.rs の X を修正して"

# claude のオプションをパススルー
wez-sidebar new ~/path -- -r

# タスク付き spawn (claude -n "<title>" でセッション名設定 + 初期プロンプト投入)
wez-sidebar new --task "DBスキーマ設計" --prompt "SQLite + Prisma で設計"
```

タブタイトルには自動でディレクトリのベース名が設定される。`--task` を指定するとタスクも自動作成され、カンバンの `Active` 列に現れる。WezTerm/tmux いずれかの環境で動作する。

## カンバン / タスク管理

複数の Claude Code セッションを並列実行するとき、依存関係とステータスを俯瞰できる。Cline Kanban にインスパイアされた機能。

### ワークフロー

```
[backlog] ── spawn ──▶ [running] ◀─── UserPromptSubmit ───┐
                          │ Stop hook                      │
                          ▼                                │
                       [review] ───────────────────────────┘
                          │ a (承認) / auto_approve
                          ▼
                       [done]  ── 依存する次タスクを自動 spawn
```

### タスク CLI

```bash
# タスク追加 (backlog)
wez-sidebar tasks add "DBスキーマ設計" --cwd ~/repo --prompt "..."

# 依存関係: B は A 完了後に開始可能
wez-sidebar tasks link <A_id> <B_id>

# 一覧 (表 or JSON)
wez-sidebar tasks list
wez-sidebar tasks list --status review --format json

# backlog → running (依存がクリアなら spawn)
wez-sidebar tasks start <id>

# review → done (+ 依存する次タスクを自動 spawn)
wez-sidebar tasks approve <id>

# review → running (追加指示する意図で戻す)
wez-sidebar tasks reject <id>

# 任意 → trash / trash → backlog
wez-sidebar tasks trash <id>
wez-sidebar tasks restore <id>

# Done タスクを claude --resume "<title>" で再開
wez-sidebar tasks resume <id>
```

### TUI 操作 (カンバンモード)

| キー | 動作 |
|------|------|
| `v` | ビュー切替 (auto ↔ kanban ↔ flat) |
| `a` | 選択中のタスクを approve (review → done) |
| `R` | 選択中のタスクを reject (review → running) |
| `T` | 選択中のタスクを trash |
| `Tab`/`h`/`l` | カラム移動 (Active / Review / Done) |
| `Space` | セクション折りたたみ (sidebar) |

### ブロック検知通知

`review` 状態が `block_alert_minutes` (デフォルト 5 分) 以上続くと、`terminal-notifier` でデスクトップ通知 (音: Basso)。クリックでペインにジャンプする。設定は下記参照。

### データ

タスクは `~/.config/wez-sidebar/tasks.json` に永続化される。Claude Code の session name (`claude -n "<title>"`) と一致しているので、`/rename` でセッション名を変えてもそのまま追跡できる。

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

### 孤児プロセスクリーンアップ（Reaper）

デフォルト無効。`config.toml` に追加して有効化:

```toml
[reaper]
enabled = true
threshold_hours = 3  # この時間を超えた孤児を kill
```

有効時、TUI が 5 分ごとに WezTerm ペインに紐づかない Claude Code プロセスを検出する。手動実行も可能:

```bash
wez-sidebar reap --dry  # kill せずに孤児を一覧表示
wez-sidebar reap        # 孤児プロセスを kill（SIGTERM）
```

### カンバン / 通知

```toml
[kanban]
auto_flat_threshold = 3         # セッション数がこれ未満なら自動で flat モード
block_alert_minutes = 5         # review 放置検知の閾値 (分)。0 で通知無効
auto_approve = false            # true で review をスキップ、Stop hook 発火で即 done 化
block_alert_sound = "Basso"     # terminal-notifier -sound の値
block_alert_cooldown_secs = 0   # 0 = review 滞在中 1 回のみ、>0 = 秒数ごとに再通知
```

## キーバインド

| キー | Sidebar | Dock |
|------|---------|------|
| `j`/`k` | 上下移動 | 上下移動 |
| `Enter` | ペイン切り替え | ペイン切り替え |
| `1`-`9` | 番号で切り替え | 番号で切り替え |
| `Tab`/`h`/`l` | カラム移動 (kanban) | カラム移動 |
| `Space` | セクション折りたたみ (kanban) | - |
| `v` | ビュー切替 (auto/kanban/flat) | ビュー切替 |
| `a` | タスク承認 (review → done) | タスク承認 |
| `R` | タスク差戻し (review → running) | タスク差戻し |
| `T` | タスク trash | タスク trash |
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
                    │ subagent 追跡        │
                    │ git branch 取得      │
                    │ yolo mode 検出       │
                    └─────────┬───────────┘
                              │
                    sessions.json + usage-cache.json
                              │
                         file watcher
                              │
                    wez-sidebar TUI（ポーリングなし）
                              │
                    reaper（オプトイン、5 分間隔）
                    └→ ps + wezterm cli list → 孤児 kill
```

すべてのデータは hook 経由で流れる。TUI はファイル変更にのみ反応し、ポーリングもサブプロセスも走らない。
reaper は `claude` プロセスと WezTerm ペインを定期比較し、孤児を検出する。

## ライセンス

MIT
