# Requirements: wez-sidebar Kanban Phase 6-8

## Background / Context

wez-sidebar のカンバン化 (docs/kanban-design.md) は Phase 1-5 まで実装済み。tasks モデル・CLI・通知・spawn・Stop hook 連携は全て完成し、cargo test 42 passed / clippy -D warnings クリーン。残り Phase 6 (UI)、Phase 7 (TUI キーバインド)、Phase 8 (ドキュメント) を実装する。

既存の前提:
- `src/tasks.rs` が `KanbanTask`, `TaskStatus`, `TasksFile` を提供
- `src/config.rs` に `[kanban]` セクション (`auto_flat_threshold` 等) が既に存在
- `src/ui.rs` (sidebar), `src/dock.rs` (dock) のレンダラ + イベントループが完成
- Session カードのレンダリング (`render_session_card`) は dock/sidebar 共通

## Functional Requirements

### FR-1: Dock モード カンバンレイアウト
- dock モードで、(active セッション + backlog/running/review/done タスクの合計) >= `kanban.auto_flat_threshold` のときカンバンビューに切り替わる
- カンバン列: **Active** (running タスク + タスク未紐付けセッション), **Review** (review タスク), **Done** (done タスク)
- 閾値未満なら既存の flat レイアウト
- backlog / trash タスクは dock カンバンに表示しない（CLI で管理）

### FR-2: Sidebar モード セクション分け
- sidebar モードで、セッション総数 >= `auto_flat_threshold` のとき Active/Review/Done の縦セクション分け
- 各セクションは折りたたみ可能（Space/Enter でトグル、選択中セクションを対象）
- 閾値未満なら既存の flat リスト

### FR-3: タスク紐付け表示
- タスクと紐付いていないセッションは **Active** 列に出す（既存通り）
- タスクと紐付いているセッション（`session_id` が `KanbanTask.session_id` と一致）は該当タスクの status を反映する
- タスクのみで session が紐付いてないもの（backlog, session_id=None の running/review/done）はタスクとしてカンバンに表示（視覚的にセッションカードと同形だが、task title を優先表示）

### FR-4: 手動 view 切替
- `v` キーで flat ⇄ kanban を手動切替
  - `k` は既存の上下ナビに使っているので使わない
- 手動切替は tick の再評価を上書きする（ユーザー選択を優先）

### FR-5: カード操作キーバインド
- カード選択中に:
  - `a` = approve (review → done + 次の依存タスク auto spawn)
  - `R` = reject (review → running、大文字で誤操作防止)
  - `T` = trash (任意 → trash、大文字で誤操作防止)
- カード ≠ session 紐付きタスクのとき (session only) は `a`/`R` は何もしない、`T` は delete_session 相当

### FR-6: ヘルプポップアップ更新
- `?` で出るヘルプに新キー (v/a/R/T) を追加
- dock/sidebar 両方で更新

### FR-7: ドキュメント更新
- `README_JA.md` と `README.md` にカンバン機能の使い方を追加:
  - `tasks` サブコマンド群の紹介
  - `new --task` の使い方
  - `config.toml` の `[kanban]` セクション
  - TUI キーバインド一覧 (v/a/R/T/?)
- `config.example.toml` に `[kanban]` の記述が既にあれば OK、なければ追加

## Non-Functional Requirements

### NFR-1: 後方互換性
- タスク 0 件の状態で今まで通り動く（flat が保たれる）
- config に `[kanban]` セクションがなくてもデフォルトで動く（既存の Default 実装で OK）

### NFR-2: 品質ゲート
- `cargo clippy --release -- -D warnings` を通過
- `cargo test --release` を通過（既存 42 test 維持 + 新規追加テストがあれば通過）

### NFR-3: 整合性
- Unicode 表示幅は `unicode-width` で計算（CLAUDE.md ルール）
- カード内テキストは `truncate_name()` を使う

## Out of Scope

- backlog タスクの TUI からの追加 (CLI で十分)
- ドラッグ&ドロップでのカード移動
- 依存関係グラフの可視化
- tasks.json の file-watching → TUI リアクティブ更新（Phase 6 では TaskUpdate は CLI または a/R/T キー経由のみ、TUI 内更新は `r` キーでの明示リロード）

## Acceptance Criteria

- [ ] AC-1: dock モードで 3 セッション+タスクあればカンバン、2 以下なら flat（auto_flat_threshold=3 の場合）
- [ ] AC-2: sidebar モードで同じ閾値でセクション分け/flat 切替
- [ ] AC-3: `v` キーで手動切替可能
- [ ] AC-4: review タスクカードで `a` を押すと done に移行、ファイルに書き込まれる
- [ ] AC-5: review タスクカードで `R` を押すと running に戻る
- [ ] AC-6: 任意のタスクカードで `T` を押すと trash に移動
- [ ] AC-7: `?` のヘルプに v/a/R/T が載っている
- [ ] AC-8: README_JA.md と README.md にカンバン章が追加されている
- [ ] AC-9: cargo clippy --release -D warnings / cargo test --release 通過
