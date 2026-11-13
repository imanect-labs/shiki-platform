# 不具合報告の GitHub 配送（PR2、#1251）

既定は `enabled=false` / `backend=disabled`。有効な disabled はローカル held 受付だけを行う。
GitHub は preview の `imanect-labs/shiki-test` 固定。production/onprem では disabled を維持する。
PR3/PR4 の本人確認・管理画面が揃うまで、この sink を運用で有効化しない。
実 GitHub の疎通は人間が明示許可した検証用 Issue だけで行う。

## App と鍵の配備

1. 組織の GitHub Settings → Developer settings → GitHub Apps で配送専用 App を作る。
   Webhook は無効。Repository permissions は **Issues: Read and write** と **Metadata: Read-only** のみ。
   他のコード・メンバー・組織権限を付与しない。
2. App を `imanect-labs/shiki-test` のみにインストールする。All repositories を選ばない。
   App ID と installation ID を控える。利用者の OAuth/PAT は使用しない。
3. Generate a private key で生成した PEM を運用の secret 管理へ保存し、サーバだけが読める
   ファイルとして mount する。リポジトリ、TOML、DB、ログ、ブラウザ、LLM に鍵本文を渡さない。
   同梱の `crates/app-gateway/tests/fixtures/test_rsa_priv.pem` はローカルスタブ専用の無効なテスト鍵であり配備に使わない。
4. `SHIKI__BUG_REPORT__BACKEND=github`、`SHIKI__BUG_REPORT__ENABLED=true` と、以下の `SHIKI__BUG_REPORT__GITHUB__` に続く項目を設定する。

| 項目 | 値 |
| --- | --- |
| `PROFILE` | `preview` |
| `OWNER` / `REPO` | `imanect-labs` / `shiki-test` |
| `APP_ID` / `INSTALLATION_ID` | 上記の数字 |
| `PRIVATE_KEY_FILE` | mount 先の絶対パス |
| `CONFIG_VERSION` | 初期値1、宛先・App installation の変更時に増加 |

Compose の任意追加例は `deploy/compose/docker-compose.bug-report-github.example.yml`。
通常 compose はこのファイルを読まない。PEM が未配備・不正なら起動を拒否する。
鍵のローテーションは、次の順序で行う。

1. GitHub App に新鍵を追加する。
2. secret ファイルを新鍵で原子的に差し替える。
3. コンテナを再作成する。
   Compose 例は単一ファイルの bind mount のため、
   `docker compose -f docker-compose.yml -f docker-compose.bug-report-github.example.yml up -d --force-recreate shiki-server`
   を実行する。単なるプロセス再起動では mount の inode は更新されない。
4. 再作成したコンテナで、新鍵を使って installation token を取得できることを確認する。
5. 確認後に旧鍵を GitHub で失効する。

token 再取得時に鍵を再読込する。
即時失効が必要なら App installation を停止し、配送サーバを再起動してメモリtokenを破棄する。
鍵・JWT・installation token は Debug/Serialize の対象にせず、外部エラー本文も記録しない。

## 配送・障害対応

submit/approve は同じDB transactionで配送IDを作成し、HTTPは実行しない。
ワーカーは15秒間隔、repo全体をDB advisory lockで直列化し一周最大8配送、lease90秒とfenceで処理する。
元の principal と HigherConsistency membership、現policy、同意、承認、宛先版を配送開始前に再確認する。
組織の共有禁止・承認ON・無効化は未着手配送を同じtransactionでheldに止める。
設定を戻してもheldを自動再開せず、本人が再確認して再提出する。

GitHub 配送時は database.max_connections を2以上にする（直列化用と処理用）。
接続失敗・token取得の一時障害・rate limit は、最大5回・24時間以内で指数backoff＋jitterを行う。
Retry-After（秒/HTTP日時）と、remaining=0 のときのrate resetの大きい方を尊重する。
rate limit 待機は installation 単位でDBに保存し、全テナント・replicaで共有する。
secondary rate limit の403はヘッダがなくても最低60秒待つ。24時間以上の待機指定は自動停止する。
401/403（rate limitを除く）/404/422は自動停止し、App installation・権限・設定を運用者が確認する。
本文・GitHub応答エラー本文を監査に入れない。監査actionはdispatch/sent/failed/reconcile。

timeout・不確定5xx・sendingのlease切れはdelivery_unknown。公開用のランダムUUID markerを
repoのIssue一覧（closedを含む、最大3ページ/300件・30秒）で照合する。
見つかれば検証済みgithub.com URLとIssue番号を保存する。見つからなければunknownのまま
1時間後に再照合する。自動再POSTはしない。運用者がGitHubでmarkerを確認し、
不明なら人間へエスカレーションする。強制再送UI・外部保持/消去・監視の整備はPR5。

外向き通信はapi.github.com/HTTPS443だけ。DNS公開IP検査後に接続を固定、環境proxy不使用、
TLS検証、redirect非追従、HTTP全体15秒（接続3秒）、応答最大1MiB。
自由記述はliteral block化し、メンション・画像・HTML・自動リンクを無害化する。
環境情報は現DTOに許可された項目が無いため「未収集」。tenant/org/user識別子は送らない。

認証仕様: [GitHub App JWT](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-json-web-token-jwt-for-a-github-app)、
[Issue API](https://docs.github.com/en/rest/issues/issues#create-an-issue)。API版は `2022-11-28` に固定する。
