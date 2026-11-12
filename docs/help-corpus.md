# ヘルプコーパスのビルドと運用

Task 12.7 / Issue #1203。原稿は private imanect-labs/shiki-hp の help/ だけに置く。
正本の変更提案（人間承認済み）として design §4.12 と phase-12 Task 12.7 を更新する。
将来の PR 本文にも「正本の変更提案」と明記する。

## CI

images.yml の server ビルドは GITHUB_TOKEN で GHCR から公開本文のみの OCI を取得する。
workflow_dispatch の help_corpus_sha に12桁の原稿 SHA を指定できる。空は main。
取得前にタグを digest に解決し、その digest で pull する。digest はイメージの
app.imanect.shiki.help-corpus.digest ラベルに残す。未公開・権限不足ではビルドを失敗させる。
package の Actions access に shiki-platform を追加する操作は管理者が行う。
同梱データは server ビルド時に ingestion-worker の推論実装で埋め込む。
既定モデル cl-nagoya/ruri-v3-30m、検索文書: / 検索クエリ: の非対称プレフィックスと L2 正規化を使う。
オンプレも同じイメージを使う。クエリの推論先は既存 RAG 設定を使う。

## ローカル

原稿リポジトリを編集せず、出力先を明示する:

```sh
CORPUS_OUT=/tmp/shiki-help-corpus npm --prefix ~/ghq/github.com/imanect-labs/shiki-hp/help/site run export:corpus
uv run --project ingestion-worker --frozen python scripts/help-corpus/embed.py /tmp/shiki-help-corpus deploy/help-corpus
```

server を起動するとき SHIKI__HELP__DATA_PATH を同梱先へ設定する。
Dockerfile は deploy/help-corpus を /opt/shiki/help へコピーする。
同梱しないローカル開発・CI は空のままで起動でき、help_search を提示しない。
稼働中の compose を再作成せず、別 project・別ポート・独立 DB/キューで検証する。
compose .env.example に有効化・パス・base URL・上限を列挙している。
base URL は origin のみ。相対 URL は /help/ 配下に限定し traversal を拒否する。

## 境界

- PIT-8: 原本とベクトルを corpus_version / chunks_sha256 で束縛し、モデル不一致では密検索を止める。
- PIT-14: 顧客本文のキャッシュ・索引を持たない。公開ヘルプの不変スナップショットのみ。
- PIT-55: 実行時に結果 URL を取得しない。利用者・LLM は URL/パス/版を指定できない。
- PIT-56: タイトル・見出し・抜粋は既存 document 封筒の内側に置き、閉じタグと仮マーカーを中和する。
- PIT-77: 画像は同梱・推論・回答描画しない。抜粋は plain text として表示する。

出典は社内文書の Citation と分離した HelpCitation、checkpoint は専用台帳を保存する。
公開ヘルプなので共有閲覧の file ReBAC 例外を追加しない。既存 doc_search の二段 authz は変更しない。
classic_rag=true の旧経路はツールを使わないため対象外。通常（既定）と自律のツール経路が対象。
実 LLM の選択品質は stub の決定的な動作確認と区別し、別途評価する。

## ローカル検証（2026-11-12）

- `cargo fmt --all --check`: pass。
- workspace 全体の `cargo clippy --all-targets --all-features -- -D warnings` は macOS で
  既存 sandbox-netns の MSG_CMSG_CLOEXEC が未定義のため失敗。
  `-p shiki-rag -p shiki-agent-core -p shiki-chat -p shiki-api` に絞った同じゲートは pass。
- `cargo nextest run -p shiki-rag -p shiki-agent-core -p shiki-chat -p shiki-llm-gateway --lib`:
  825 passed、0 skipped。
- `SHIKI_REQUIRE_IT=1` と独立 DB / 実 OpenFGA を設定し、`search_authz_it`: 7 passed、0 skipped
  （role_membership_grants_visibility_through_folder は初回失敗・nextest の再試行で成功）。
- 同じ厳格設定で chat の `viewer_stream_it` / `checkpoint_it`: 7 passed、0 skipped。
  専用ヘルプ出典の保存・共有閲覧・SSE 再送を含む。
- Homebrew bash で `scripts/check-file-size.sh`、migration 番号、`cargo machete crates`、
  `python3 scripts/ci-select.py check`: pass。
- Web の frozen install、`pnpm gen:api`、lint、build: pass。
- `uvx ruff check scripts/help-corpus`、`python3 -m unittest discover -s scripts/help-corpus -v`: pass。
- shiki-hp のローカル export は484ページ・3735チャンク。稼働中 ingestion-worker と同じ
  model_registry の document 推論で実ベクトルを作り、専用 DB・redis DB 9・server :18080 で起動。
  既存 compose project は再作成していない。
- `HELP_CORPUS_E2E=1 E2E_BASE_URL=http://localhost:3000 pnpm --dir web exec playwright test e2e/help-search.spec.ts`:
  1 passed。初回は検証 server の再起動と競合して login timeout、再起動完了後に成功。
  `[H1]`・パスキー追加ページの節リンク・再訪後の保持を確認。
  スクリーンショットは `web/test-results/help-search.png`（ローカル生成物・git 対象外）。

未実行: GHCR 実取得から Docker image publish までの通し（上流コーパス未 publish）、
オンプレ実機、実 LLM のツール選択品質・回答品質、全 Web E2E、カバレッジ、上記以外の結合テスト。
共有 Qdrant の alias を変更するテストは稼働環境への影響を避けるため未実行。
LLM は stub の明示 `helpsearch:` 駆動であり、選択品質の評価を代替しない。
将来の PR 本文にはこの結果と未実行項目、および「正本の変更提案」を転載する。
