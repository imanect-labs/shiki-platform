---
name: dev-workflow
description: shiki-platform の開発タスクの進め方（ブランチ作成 → issue 化 → 実装 → PR 作成 → issue クローズ）。タスクに着手するとき、PR を作るとき、roadmap のフェーズタスクを実装するときに使う。
---

# 開発フロー

shiki-platform の開発タスクを進める標準手順。docs/roadmap.md の各タスク = 1 GitHub Issue が原則。

## 1. 着手前

- 必ずブランチを切る。main で直接作業しない。ブランチ名はタスク内容が分かる名前にする。
- human から issue URL が指定されていない場合は、着手前にタスク内容を Issue 化する。
  - ラベル: 領域に応じた area:*（auth / storage / rag / chat / sandbox / agent / gateway / data / web / obs など）。
- どのフェーズ・どの依存に属するタスクか docs/roadmap.md で確認する（依存: 認証→ストレージ→RAG→チャット→サンドボックス→…）。

## 2. 実装中

- 縦スライス（API → サービス → ストレージ → フロント）で動く状態を保つ。
- architecture-invariants スキルの不変条件を守る（単一チョークポイント / AuthContext / 二段authz / トレイト境界 / codegen）。
- コミットは意味のある単位で、メッセージは日本語・命令形で簡潔に（例: feat(storage): フォルダ共有のReBACタプル付与を追加）。

## 3. 完了時

- ローカル/CI のチェックを通す: cargo fmt --check / cargo clippy -- -D warnings / cargo test、web は pnpm lint / pnpm build、compose smoke。
- PR を作成する（/pull-request スキルが利用可能ならそれを使う）。PR 本文には目的・対応 Issue・検証方法を書く。
- 対応する Issue に close コメントを付けてクローズする（PR 説明に Closes #<n> を含める）。

## 注意

- 判断に迷ったら必ず human に相談する。特に OpenFGA の relation schema（ポリシ決定）・優先順位・トレイト境界は人が握る領域。
- PR は human が明示的に依頼した場合に作成する。勝手に外部へ公開する操作は確認を取る。
