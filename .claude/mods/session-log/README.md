# session-log（Claude Code mod）

Claude Code のセッション中に送ったプロンプト、作成した Issue、作成した Pull Request を一覧表示する mod。
開発者個人の作業補助用で、プロダクト（shiki-platform 本体）とは無関係。

## 使い方

- `/session-log` … 一覧を Markdown で会話内に表示する（スマホ版など、ペインを置けない画面でも見られる）。
- ペインを置ける画面（ターミナル・デスクトップ）では、右側に「セッション履歴」ペインも開く。

一覧はセッションの会話記録から毎回集計し直すため、mod が後から読み込まれても、それ以前の分も含まれる。

| 区分 | 拾う操作 |
| --- | --- |
| Pull Request | `mcp__github__create_pull_request`、`gh pr create` |
| Issue | `mcp__github__issue_write`（`method: create`）、`gh issue create` |
| プロンプト | 人が送ったメッセージ（システム差し込み・スキル本文・タスク通知は除く） |

結果に GitHub の URL が含まれない（失敗・実行中）の作成は載せない。

## 読み込ませ方

リポジトリ内の `.claude/skills/` やプロジェクト設定の `enabledPlugins` からは、hooks モジュールの mod は自動では読み込まれない。
ユーザー側の `~/.claude/skills/<name>/` に置くと読み込まれるので、セッション開始前にコピーする。

- **Claude Code on the web（クラウド環境）:** 環境のセットアップスクリプトに以下を入れる。

  ```bash
  if [ -d shiki-platform/.claude/mods/session-log ]; then
    mkdir -p ~/.claude/skills && cp -R shiki-platform/.claude/mods/session-log ~/.claude/skills/
  fi
  ```

- **ローカル:** `cp -R .claude/mods/session-log ~/.claude/skills/`（または `claude --plugin-dir .claude/mods/session-log`）。

## 検証

```bash
claude plugin validate .claude/mods/session-log
```
