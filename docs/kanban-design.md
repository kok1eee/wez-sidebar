# wez-sidebar カンバン化 設計書

**作成日**: 2026-04-07  
**対象バージョン**: v0.4.0 (想定)  
**ステータス**: Draft

---

## 1. 背景

Cline Kanban (https://github.com/cline/kanban) のように、**複数の Claude Code セッションをカンバン方式で一括管理する** アプローチが注目されている。AI コーディングエージェントを並列実行する際、人間がボトルネックとなって:

- ブロック (許可待ち・入力待ち) に気づかず放置
- 完了タスクに気づかず放置
- ターミナル間の往復・コンテキストスイッチコスト

といった認知的負荷が蓄積する問題が知られている。

wez-sidebar は既に「複数 Claude Code セッションの監視」機能を持っているが、**タスク管理**・**ステータス別ビュー**・**ブロック通知** を加えることで、Cline Kanban と同等の UX を WezTerm 内で実現できる。

本設計書では、これらの機能追加の全体像と実装方針を定義する。

---

## 2. 目的

1. **タスク管理**: タスクをバックログに積み、依存関係を張り、一覧できる
2. **自動 spawn**: タスクを元に Claude Code セッションを起動 (`claude -n "<title>"` + 初期プロンプト投入)
3. **カンバンビュー**: Active / Waiting / Review / Done をステータス別に可視化
4. **ブロック検知通知**: 入力待ちが一定時間以上続いたら macOS 通知
5. **完了→次起動**: review カラムでユーザーが承認したら、依存する次のタスクを自動 spawn

---

## 3. 非目標 (Out of scope)

以下は意図的にスコープ外とする:

| 項目 | 理由 |
|---|---|
| Git worktree 自動管理 | Claude Code の `--worktree` フラグで十分 |
| コード diff プレビュー / コメント機能 | TUI では中途半端になる。`jj diff` 直接で十分 |
| 複数ホスト対応 | 一人プロジェクト想定 |
| Claude Code 以外のエージェント対応 | 既存 backend 抽象は WezTerm/tmux 二択に留める |
| Web UI | TUI のみ |

---

## 4. ユーザーストーリー

| ID | 内容 |
|---|---|
| US-1 | ユーザーは `wez-sidebar tasks add "<title>"` でバックログにタスクを追加できる |
| US-2 | ユーザーは依存関係を張り、「A 完了後に B を実行」を宣言できる |
| US-3 | ユーザーは `wez-sidebar new --task "<title>"` で即座にタスクを spawn できる |
| US-4 | ユーザーは TUI のカンバンビューでタスクの進捗を俯瞰できる |
| US-5 | ユーザーは 5 分以上ブロックされたタスクについて macOS 通知で気づける |
| US-6 | ユーザーは review カラムで完了タスクを承認すると、依存する次タスクが自動起動する |

---

## 5. データモデル

### 5.1 新規ファイル: `~/.config/wez-sidebar/tasks.json`

```jsonc
{
  "tasks": [
    {
      "id": "abc12345",               // 短縮 UUID (8桁)
      "title": "DBスキーマ設計",       // claude -n に渡す値 & 表示名
      "prompt": "SQLite + Prisma で...", // spawn 時に送信する初期プロンプト (optional)
      "status": "backlog",            // 後述のステータス
      "cwd": "/Users/me/repo",        // 作業ディレクトリ
      "session_id": null,             // 実行中のみ埋まる (Claude Code の session_id)
      "created_at": "2026-04-07T19:00:00Z",
      "started_at": null,
      "completed_at": null
    }
  ],
  "dependencies": [
    { "from": "abc12345", "to": "def67890" }  // from が完了したら to が起動可能
  ]
}
```

### 5.2 既存ファイルへの変更

`sessions.json` には変更を加えない (Claude Code hook が書くものなので触らない)。

タスクとセッションの紐付けは **Claude Code session name (`claude -n "<title>"` の値)** 経由で行う:

- `new --task "<title>"` は `claude -n "<title>"` で起動する
- Session の name が tasks.json の title と一致することで逆引き可能
- session_id が確定したタイミング (UserPromptSubmit hook) で tasks.json に session_id を書き戻す

### 5.3 ステータス定義

| status | 意味 | 主な遷移元 |
|---|---|---|
| `backlog` | バックログ (未実行) | 新規作成 |
| `running` | Claude Code が能動的に動作中 (ツール実行中など) | backlog → (spawn)、review → (UserPromptSubmit) |
| `review` | Claude Code が応答を終えて入力待ち | running → (Stop hook) |
| `done` | ユーザーが承認した完了タスク | review → (承認) |
| `trash` | 削除済み | 任意 → (trash 操作) |

※ 「許可待ち (permission_prompt)」は **running のサブ状態** として扱い、タスクの status は `running` のまま保つ。通知は既存の `send_permission_notification` で行われる。

### 5.4 ステータス遷移図

```
     [backlog]
         │ spawn
         ▼
     [running] ◀──────────────┐
         │ Stop hook           │ UserPromptSubmit hook
         ▼                     │
     [review] ────────────────┘
         │ approve (手動 / auto_approve)
         ▼
      [done]
         │
         └─ (依存先の次タスクを自動 spawn)

任意の状態 → [trash]   (削除操作)
```

---

## 6. モジュール構成

### 6.1 新規モジュール

| ファイル | 責務 |
|---|---|
| `src/tasks.rs` | Task/Dependency 型、tasks.json の読み書き、依存解決 |
| `src/notify.rs` | 通知ロジックの集約 (既存の permission 通知もここに移動) |

### 6.2 既存モジュール改修

| ファイル | 改修内容 |
|---|---|
| `src/types.rs` | `Task`, `TaskStatus`, `Dependency` 型を追加 |
| `src/main.rs` | `new --task` フラグ、`tasks` サブコマンド群 |
| `src/hooks.rs` | Stop hook で review 遷移、UserPromptSubmit で running 戻し、session_id 書き戻し |
| `src/ui.rs` | カンバンビューレンダリング、flat/kanban 切替 |
| `src/dock.rs` | カンバンビュー (横列) |
| `src/app.rs` | App に `tasks: Vec<Task>`, `view_mode: ViewMode` 追加 |
| `src/config.rs` | `[kanban]` セクションを追加 |
| `src/session.rs` | `send_permission_notification` を `notify.rs` へ移動 |

---

## 7. UI レイアウト

### 7.1 dock モード (横長、leader+s で呼び出し想定)

**セッション数 ≥ 3 (デフォルト閾値) のときカンバン**:

```
┌─ Active (3) ────────────┬─ Review (1) ──┬─ Done (2) ──┐
│ [repo-a]  [repo-b]      │ [repo-d]      │ [repo-e]    │
│ [repo-c]                │               │ [repo-f]    │
└─────────────────────────┴───────────────┴─────────────┘
```

**セッション数 < 3 のとき flat (既存 UI)**:

```
┌─ Sessions (2) ───────────────────────────────────────┐
│ [repo-a]  [repo-b]                                   │
└──────────────────────────────────────────────────────┘
```

キー `k` で手動切替 (強制カンバン ↔ 強制 flat)。

### 7.2 sidebar モード (縦長、狭い)

**セッション数 ≥ 3 のときセクション分け**:

```
▼ Active (3)
  [repo-a] running
  [repo-b] thinking  
  [repo-c] running
▼ Review (1) 🟡
  [repo-d] 5分放置
▶ Done (2) (折りたたみ済)
```

セクションは折りたたみ可能 (`Space` or `Enter` でトグル)。

### 7.3 表示要素

各カードには以下を表示 (既存):

- タスク title (tasks.json と紐付いていれば) または session 名
- ステータス (running / review 等)
- 経過時間 (review に入ってからの時間、waiting_input 時間など)
- cwd (末尾のみ)
- 直近のアクティビティ

新規追加:

- タスクとの紐付きマーク (タスクなしのセッションと区別)
- 依存関係 (依存しているタスクの短い表示、例: `← abc12345`)

---

## 8. フック連携

### 8.1 既存フックとの連携

| hook event | 既存処理 | 新規処理 |
|---|---|---|
| `UserPromptSubmit` | sessions.json 更新 | タスク status を running に、session_id を書き戻す |
| `Stop` | sessions.json 更新 (waiting_input) | タスク status を review に遷移 |
| `Notification` (permission_prompt) | permission 通知発火 | 変更なし (通知は既存) |
| `PreToolUse` / `PostToolUse` | sessions.json 更新 | 変更なし |

### 8.2 ブロック検知

TUI のバックグラウンドスレッド (またはメインループの tick 内) で定期チェック:

```
全タスクを走査
  → status = review で、review_started_at + threshold < now
    → 未通知 かつ alert 有効なら terminal-notifier 発火 + 通知フラグセット
  → status が review 以外に戻ったら通知フラグリセット
```

閾値は `config.toml` の `block_alert_minutes` (デフォルト 5)。

### 8.3 完了→次タスク自動 spawn

```
タスクが review → done に遷移した時 (手動 approve または auto_approve)
  → dependencies を走査し、from = このタスク.id の to を取得
  → to タスクの他の依存 (from が別のタスク) がすべて done か確認
  → すべて done なら、to タスクを spawn (status = running)
```

---

## 9. CLI API

### 9.1 タスク関連の新規サブコマンド

```bash
# 追加
wez-sidebar tasks add "<title>" [--cwd <path>] [--prompt "<prompt>"] [--depends-on <id>]

# 一覧
wez-sidebar tasks list [--status <status>] [--format table|json]

# 依存関係
wez-sidebar tasks link <from_id> <to_id>
wez-sidebar tasks unlink <from_id> <to_id>

# 状態操作
wez-sidebar tasks start <id>      # backlog → running (spawn する)
wez-sidebar tasks approve <id>    # review → done
wez-sidebar tasks reject <id>     # review → running (ユーザーが追加指示する意図)
wez-sidebar tasks trash <id>      # 任意 → trash (戻すには restore)
wez-sidebar tasks restore <id>    # trash → backlog
wez-sidebar tasks edit <id> [--title ...] [--prompt ...]  # 編集

# 再開
wez-sidebar tasks resume <id>     # done タスクを claude --resume "<title>" で再開
```

### 9.2 既存サブコマンドの拡張

```bash
# new サブコマンドに --task を追加
wez-sidebar new [dir] [-w] --task "<title>" [--prompt "<prompt>"] [-- <claude_args>]

# 意味:
#   1. tasks.json にタスクを追加 (backlog)
#   2. 即座に claude -n "<title>" で spawn
#   3. 初期プロンプトがあれば ペインに send-text で投入
#   4. タスクを running に遷移
```

---

## 10. 設定 (`config.toml`)

```toml
# 既存セクション ...

[kanban]
# カンバン/flat 自動切替の閾値 (これ未満なら flat)
auto_flat_threshold = 3

# ブロック検知通知の閾値 (分)
block_alert_minutes = 5

# review をスキップして done に即遷移 (Cline Kanban 式の自動パイプライン)
auto_approve = false

# ブロック通知の音 (terminal-notifier -sound)
block_alert_sound = "Basso"

# ブロック通知の連打防止: 同じタスクへの再通知を抑制する秒数 (0 で無制限抑制)
block_alert_cooldown_secs = 0
```

---

## 11. 実装順序 (Phase)

規模が大きいため、段階的に実装する。各 Phase は独立してマージ可能な単位とする。

### Phase 1: タスクモデル基盤

- `types.rs`: Task, TaskStatus, Dependency 型追加
- `tasks.rs`: tasks.json 読み書き、依存解決ロジック
- 単体テスト

**成果物**: `wez-sidebar tasks list` が空リストを返す状態

### Phase 2: タスク CLI

- `main.rs`: `tasks add/list/link/unlink/trash/restore/edit/approve/reject/resume`
- バックログ管理がコマンドラインで完結する

**成果物**: CLI だけでタスク追加・依存設定・一覧表示できる

### Phase 3: ブロック検知通知

- `notify.rs` 新設、`send_permission_notification` を移動
- `block_alert` 機能の追加 (TUI tick 内で判定)
- `config.toml` の `[kanban]` セクション対応

**成果物**: 5 分放置で macOS 通知が出る (タスク機能と独立に sessions.json ベースで動く)

### Phase 4: タスク spawn 統合

- `main.rs`: `new --task` フラグ追加
- `claude -n "<title>"` での spawn
- 初期プロンプトを `wezterm cli send-text` / `tmux send-keys` でペインに投入
- `wezterm cli send-text` を使う `terminal.rs::send_text` メソッドを backend trait に追加
- `hooks.rs`: UserPromptSubmit 時に title → task_id 逆引き、session_id 書き戻し、status=running

**成果物**: `wez-sidebar new --task "<title>" --prompt "<prompt>"` で spawn、タスクが running 状態に

### Phase 5: Stop hook → review 遷移 + 自動 spawn

- `hooks.rs`: Stop hook でタスク status を review に
- 承認で done、依存先の次タスクを自動 spawn
- `auto_approve` オプション対応

**成果物**: タスクチェーンが自動で回る

### Phase 6: カンバン UI (TUI)

- `ui.rs` / `dock.rs`: カンバンレイアウト実装
- セッション数で flat/kanban 自動切替
- `k` キーで手動切替
- sidebar の縦セクション分け + 折りたたみ

**成果物**: TUI 上でカンバンが見える

### Phase 7: タスク操作 TUI (キーバインド)

- カードにフォーカスして `a` = approve, `r` = reject, `t` = trash など
- タスク追加は TUI からできなくても良い (CLI で十分)

**成果物**: TUI 上でカンバン操作が完結する

### Phase 8: ドキュメント・仕上げ

- README_JA.md / README.md 更新
- config.example.toml にサンプル追記
- clippy + 動作確認

---

## 12. 未解決事項 / TODO

| 項目 | 検討内容 |
|---|---|
| **タスクと session の紐付けタイミング** | UserPromptSubmit 時に title 一致で逆引きする案だが、複数タスクが同じ title を持つ可能性もある (β 案なので) → 最新の pending タスクを優先する？ |
| **`--resume` との関係** | Claude Code が `--resume` で起動した場合、その session は既存タスクの続きなのか、新規なのか判断困難 |
| **依存グラフの循環検出** | tasks link 時に循環していないかバリデーションが必要 |
| **`tasks list` の表示順** | 依存グラフのトポロジカルソート? 作成日時順? ステータス順? |
| **空カラムの扱い** | カンバンで Review が 0 件のとき、カラム自体を非表示にするか、「(0)」で残すか |
| **タスクのリトライ** | reject したタスクはプロンプトを再送するか、ユーザーが追加入力するのを待つか |
| **Git worktree との併用** | Claude Code の `--worktree` フラグと `new --task` を組み合わせるケース |
| **tasks.json の lock** | wez-sidebar プロセスと hook プロセスが同時に tasks.json を書く可能性 → fs2 crate で advisory lock?  |

---

## 13. 参考

- [Cline Kanban](https://github.com/cline/kanban)
- [Claude Code Changelog v2.1.76-2.1.92](https://code.claude.com/docs/en/changelog)
- 既存実装: `src/hooks.rs`, `src/session.rs`, `src/dock.rs`
- 関連コミット: `afeb873` (new サブコマンド追加)
