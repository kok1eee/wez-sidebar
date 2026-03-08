# wez-sidebar - Project Configuration

## プロジェクト概要

WezTerm 用の Claude Code セッション監視サイドバー。複数の Claude Code エージェントセッションを一覧表示し、ステータス・使用量・アクティビティをリアルタイムで追跡する Rust 製 TUI ツール。

## 技術スタック

- **言語**: Rust
- **TUI**: ratatui + crossterm
- **ファイル監視**: notify crate
- **設定**: TOML (config.toml)
- **データ永続化**: JSON (sessions.json, usage-cache.json)
- **バージョン管理**: Jujutsu (jj)

## アーキテクチャ

```
Claude Code hooks → sessions.json → file watcher → TUI 更新
                                  ↘ usage-cache.json → Usage 表示
```

- `src/hooks.rs` — hook イベント処理（PreToolUse / PostToolUse / Notification / Stop / UserPromptSubmit）
- `src/ui.rs` — サイドバーモード TUI + イベントループ
- `src/dock.rs` — ドックモード TUI（横並びカード）
- `src/session.rs` — セッションデータ読み書き・WezTerm CLI 操作
- `src/app.rs` — アプリケーション状態
- `src/types.rs` — 共通型定義
- `src/config.rs` — 設定読み込み
- `src/usage.rs` — Anthropic 使用量キャッシュ

## 開発ガイドライン

- **ビルド & インストール**: `cargo install --path .`
- **コード品質**: `cargo clippy -- -D warnings` を必ず通すこと
- **コミット**: Conventional Commits 形式 (`feat:`, `fix:`, `refactor:` など)
- **VCS**: `jj` を使用（git コマンドは使わない）
- Unicode 表示幅は `unicode-width` crate で計算（`chars().len()` は使わない）
- カード内テキストは `truncate_name()` で必ずクリップすること
