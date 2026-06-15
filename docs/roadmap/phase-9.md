# Phase 9 — ミニアプリ／業務アプリ基盤

> 目的: Phase 6（A=宣言的ミニアプリ）の上に **B=コードベース・ミニアプリ**と**業務アプリ中核（構造化データ＋
> ワークフロー）**を足し、「社内業務アプリが自然増殖する基盤」を立ち上げる。中核は **out-of-trust な
> ミニアプリから内部APIをセキュアに叩く仕組み** ＝ ①公開APIゲートウェイ（唯一の入口・能力面再公開）
> ②ユーザー委譲OAuth2(PKCE)＋Keycloak再利用 ③二重ゲート（スコープ ∩ ユーザーReBAC）。
> 汎用PaaS/DBaaSは作らず「管理データサービス＋既存サンドボックス再利用＋公開API」の3点で構成する。
> 完了の定義(DoD): shikiチーム/パワーユーザーが、構造化データ（テーブル/フィールド/一覧/細粒度権限）と
> ワークフロー（承認フロー）を持つコードベース・ミニアプリをマニフェストで定義し、レジストリにpublish→
> 管理者が同意してインストール→所有テーブルが自動プロビジョンされ部署で実行できる。ミニアプリは別オリジン/CSP
> （B1）またはサンドボックス（B2）で隔離実行され、shiki機能（storage/data/rag/ai/identity/events）は
> 公開APIゲートウェイ＋スコープ付きトークン経由でのみ呼べ、毎呼び出しが `スコープ ∩ ユーザーReBAC` で認可され監査される。
> ミニアプリ内から `agent.invoke` でAI（ツール＋RAG）を呼べ、RAGは個人ReBACで再チェックされる。

## タスク一覧

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 9.1 | ミニアプリ・アーティファクト拡張（マニフェスト＋kind=mini_app_code） | app | 6.1, 6.10 |
| 9.2 | 構造化データサービス: record／スキーマレジストリ＋式インデックス | data | 6.1 |
| 9.3 | 行レベル認可述語エンジン（WHERE強制注入・クエリコンパイラ・集計適用） | data | 9.2, 6.1 |
| 9.4 | 宣言的クエリ／保存ビュー／集計＋フィールドマスク | data | 9.3 |
| 9.5 | レコード・リビジョン履歴＋楽観ロック（rev） | data | 9.2 |
| 9.6 | 公開APIゲートウェイ(BFF): 能力面・スコープ検証・per-call OpenFGA（二重ゲート） | api | 9.2, 3.9 |
| 9.7 | OAuth2クライアント登録＆PKCE／token-exchange（Keycloak連携） | auth | 9.6 |
| 9.8 | 能力アダプタ: storage/data/rag/identity/events を能力面に薄く公開 | api | 9.6 |
| 9.9 | ミニアプリ内AI: llm.invoke／agent.invoke＋コスト計上＋ガードレール | ai | 9.6, 6.9, 5.1 |
| 9.10 | ワークフロー: 軽量FSMエンジン（状態／遷移認可／副作用／監査） | data | 9.3, 6.5 |
| 9.11 | B1ランタイム: 別オリジン配信＋CSP＋ブラウザOAuth＋レンダラ統合 | frontend | 9.7, 9.8, 6.6 |
| 9.12 | B2ランタイム: サンドボックス上アプリ関数実行＋egress allowlist＋confidential client | sandbox | 9.7, 4.1 |
| 9.13 | 配布: レジストリ／同意インストール／所有テーブルプロビジョン／信頼ティア／署名 | infra | 9.1, 9.7 |
| 9.14 | ミニアプリ SDK＋CLI（shiki app init/dev/publish）＋公開API型配布 | frontend | 9.8, 9.13 |
| 9.15 | ミニアプリ基盤の監査計装（ゲートウェイ認可／行authz／FSM遷移／AI） | obs | 9.6, 9.10, 6.12 |

---

## 詳細

### Task 9.1: ミニアプリ・アーティファクト拡張（マニフェスト＋kind=mini_app_code）
- **area**: app / **path**: `crates/app-platform`, migrations
- **依存**: 6.1, 6.10
- **仕様**:
  - Phase 6.10 の `artifact(kind=mini_app)` を拡張し、**コードベース・ミニアプリ**を表す `kind=mini_app_code` を追加。
  - **マニフェスト**を artifact_version の body に持つ: `name/version`・要求スコープ・所有テーブル/スキーマ定義参照・
    ワークフロー定義参照・許可モデル/予算・フロントバンドル参照（ObjectStore）・（B2なら）サーバコード参照とエントリポイント・信頼ティア。
  - 既存の version＋ReBAC＋監査共通枠（6.1）にそのまま乗せる。A（宣言的）とBは同一テーブル・同一共有API。
  - マニフェスト検証は **authz語彙のSingle Source of Truth（design §4.1 codegen語彙）に照合**:
    要求スコープ・許可ツール・参照アクションIDが**閉じた語彙集合に存在するもののみ許可**（LLM/開発者由来の実在しない権限名を拒否）。
- **受け入れ条件**:
  - [ ] マニフェスト付き mini_app_code アーティファクトを作成・新バージョン追記でき、過去バージョンが不変で取れる
  - [ ] マニフェストのスキーマ検証（要求スコープ・所有テーブル・エントリ）が効き、不正は拒否される
  - [ ] 存在しないスコープ/ツール/アクションIDを参照するマニフェストが語彙照合で拒否される
  - [ ] A（宣言的）と B（コード）が同じ共有・バージョン・監査経路に乗る

### Task 9.2: 構造化データサービス（record／スキーマレジストリ＋式インデックス）
- **area**: data / **path**: `crates/data`, migrations
- **依存**: 6.1
- **仕様**:
  - 既存Postgres上の管理サービス。`record(id, table_id, org, data JSONB, rev, owner, created_at, updated_at)` ＋
    `table_schema(id, app_id, fields[], validations)`。**ランタイムDDLを打たない**（行は共有テーブルにJSONBで格納）。
  - フィールド型: text/number/date/datetime/select/multi-select/**user参照/dept参照/file参照(storage)/record参照/lookup/計算**。
  - スキーマで宣言されたフィルタ/ソート対象フィールドに **JSONB式インデックス**を生成。書込時にサーバ検証（型・必須・unique・参照整合）。
  - テーブル＝OpenFGA `data_table` 型（viewer/editor/owner）。Q6の第1層ReBAC。生DBはアプリに一切公開しない。
- **受け入れ条件**:
  - [ ] スキーマを定義してテーブルを作成し、型検証付きでレコードCRUDできる
  - [ ] 宣言フィールドに式インデックスが張られフィルタ/ソートが効く
  - [ ] user/dept/file/record 参照型が解決され整合性検証される

### Task 9.3: 行レベル認可述語エンジン（WHERE強制注入・クエリコンパイラ・集計適用）
- **area**: data / **path**: `crates/data`, `crates/authz`
- **依存**: 9.2, 6.1
- **仕様**:
  - テーブル定義の宣言的 `row_policy`（read/write、`$user.id/$user.dept(subtree)/$user.role/$user.groups` を参照可）を、
    クエリ時に**省略不可・上書き不可のWHERE述語**へコンパイルしANDで強制付与。クライアントは生SQLを送れない。
  - 述語は**集計（count/sum/avg）にも適用**し、非可読行が件数/合計から漏れない（permission-aware と同一保証）。
  - 述語キー（owner/dept/status等）は式インデックス前提。**個別共有はスパースに OpenFGA tuple へ逃がし** `OR id IN (共有id)` を付与。
- **受け入れ条件**:
  - [ ] row_policy で「自分/自部署/公開」等の行だけが返り、他は取得・集計の双方で除外される
  - [ ] クライアント指定フィルタに何を渡してもauthz述語はバイパスできない
  - [ ] 個別共有した特定レコードだけ追加で見え、tuple数が共有件数に比例（全件には載らない）

### Task 9.4: 宣言的クエリ／保存ビュー／集計＋フィールドマスク
- **area**: data / **path**: `crates/data`, `crates/app-platform`
- **依存**: 9.3
- **仕様**:
  - 宣言的クエリAPI（filter/sort/page/aggregate）。**生SQL非公開**。クエリは 9.3 の述語と必ず合成して実行。
  - **保存ビュー**（一覧/グラフ/カレンダー相当）を artifact 化（6.1枠・ReBAC共有・バージョン）。
  - **フィールドマスク**（field_policy）: 行が見えても特定フィールドをロールで非可視化（取得後マスク）。
- **受け入れ条件**:
  - [ ] filter/sort/page/aggregate がauthz述語と合成されて正しい結果を返す
  - [ ] 保存ビューを作成・共有・バージョン切替でき、実行時に述語が効く
  - [ ] field_policy 対象フィールドが無権限ロールには返らない

### Task 9.5: レコード・リビジョン履歴＋楽観ロック
- **area**: data / **path**: `crates/data`, migrations
- **依存**: 9.2
- **仕様**:
  - レコード更新を**追記型 changelog**（誰が・いつ・どのフィールドをどう変えたか）で記録。過去リビジョン取得可。
  - `rev` による楽観ロック（同時更新の衝突検出→409）。
- **受け入れ条件**:
  - [ ] 更新ごとにリビジョンが残り、フィールド単位の差分を辿れる
  - [ ] 競合する同時更新が検出され安全に拒否される
  - [ ] 履歴取得が 9.3 の認可（行/フィールド）に従う

### Task 9.6: 公開APIゲートウェイ(BFF) — 能力面・スコープ検証・per-call OpenFGA（二重ゲート）
- **area**: api / **path**: `crates/app-gateway`
- **依存**: 9.2, 3.9
- **仕様**:
  - 内部APIを直接公開せず、**唯一の入口**としてキュレーション済み・バージョン付き・スコープ付きの能力面を再公開。
  - 各リクエストで: ①アクセストークン検証 → ②要求スコープがアプリ付与スコープ＋リソース束縛内か確認 →
    ③**呼出ユーザーのReBACを OpenFGA で per-resource 判定**。**実効 = スコープ ∩ ユーザーReBAC**（二重ゲート）。
  - レート制限/クォータ（(ユーザー×アプリ)）、破壊系/高コスト系は 3.9 の明示許可ポリシを継承。
- **受け入れ条件**:
  - [ ] 内部エンドポイントへゲートウェイを介さず到達できない
  - [ ] アプリが広いスコープでも、ユーザー非可読リソースには到達できない（二重ゲート）
  - [ ] スコープ外/未宣言リソースへのアクセスが拒否され監査に残る

### Task 9.7: OAuth2クライアント登録＆PKCE／token-exchange（Keycloak連携）
- **area**: auth / **path**: `crates/app-gateway`, `crates/authz`
- **依存**: 9.6
- **仕様**:
  - 各ミニアプリ = Keycloak の OAuth2 クライアント（レジストリ登録と連動）。**新規認証基盤は作らない**。
  - **B1=public client**（authcode+PKCE・secretなし・短命トークン）。**B2=confidential client**（secretはサンドボックス内、
    ユーザー操作は **token-exchange / on-behalf-of** でユーザー代理を維持）。
  - **自動化のみ**: 所有データ(`data.app:*`)限定の狭スコープ・サービスidentityを発行（ReBACで所有データに束縛）。
- **受け入れ条件**:
  - [ ] B1 が PKCE でトークンを取得しゲートウェイを叩ける（client secret 不要）
  - [ ] B2 が token-exchange で「ユーザーの代理」として認可される（アプリ単独権限に昇格しない）
  - [ ] 自動化サービスidentityが所有データのみに限定され、越境が拒否される

### Task 9.8: 能力アダプタ（storage/data/rag/identity/events を能力面に薄く公開）
- **area**: api / **path**: `crates/app-gateway`, `crates/api`
- **依存**: 9.6
- **仕様**:
  - 能力カタログを `<能力>.<操作>` で実装: `storage.read/write`・`data.read/write/schema`・`rag.query`・
    `identity.read`・`events.subscribe`/`notify.send`。各操作はリソース束縛と per-call OpenFGA に従う。
  - `rag.query` は permission-aware（個人ReBAC再チェック）。`identity.read` は最小限（id/部署/ロール）。
- **受け入れ条件**:
  - [ ] 各能力が宣言スコープ＋ReBACの範囲でのみ動作する
  - [ ] `rag.query` がアプリ経由でもユーザー非可読文書を返さない
  - [ ] 能力ごとに利用量・コストが (ユーザー×アプリ) で計上される

### Task 9.9: ミニアプリ内AI（llm.invoke／agent.invoke＋コスト計上＋ガードレール）
- **area**: ai / **path**: `crates/app-gateway`, `crates/agent-core`, `crates/llm-gateway`
- **依存**: 9.6, 6.9, 5.1
- **仕様**:
  - **raw `llm.invoke`**（アプリがプロンプト供給）＋ **`agent.invoke`**（agent-core起動・ツール実行＋RAG＋構造化データ参照）。
  - `agent.invoke` のツール = **アプリ宣言ツール ∩ template ∩ ユーザーReBAC**。コンテキストにアプリデータ/RAGスコープを渡せるが
    **RAGは個人ReBACで再チェック**。モデル/トークン予算ガードレール（アプリ登録時宣言＋管理者キャップ）。
  - コストは (ユーザー×アプリ) で計上、llm-gateway 経由で Langfuse に `app_id+user+trace_id`。SSEストリーミングを通す。
- **受け入れ条件**:
  - [ ] ミニアプリ内から llm.invoke / agent.invoke が呼べ、結果がストリーミングされる
  - [ ] agent.invoke のツール/RAGがユーザーReBACで絞られ、非可読データが混入しない
  - [ ] モデル/予算上限が効き、利用量が (ユーザー×アプリ) で記録される

### Task 9.10: ワークフロー（軽量FSMエンジン）
- **area**: data / **path**: `crates/app-platform`, `crates/data`
- **依存**: 9.3, 6.5
- **仕様**:
  - テーブルに紐づく宣言的FSM（states/transitions）を artifact 化。状態 = レコードの `status` フィールド。
  - **遷移認可 = 9.3 の述語エンジンを再利用**（`actor: field:承認者 || role:部長` 等）。サーバ強制で定義外ジャンプ不可。
  - 副作用 = 宣言的アクションのみ（通知/フィールド自動設定/`agent.invoke`）。条件分岐・並列承認（全員/誰か）まで。
  - 担当者は user/dept/role/フィールド動的。保留中タスクを events 経由で「マイタスク」に。
- **受け入れ条件**:
  - [ ] 承認フロー（申請→承認→完了/差戻し）が定義どおり遷移し、定義外遷移が拒否される
  - [ ] 遷移認可が行述語で評価され、権限のない人は遷移できない
  - [ ] 全遷移がリビジョン履歴＋監査に残り、status が行可視性を駆動する

### Task 9.11: B1ランタイム（別オリジン配信＋CSP＋ブラウザOAuth＋レンダラ統合）
- **area**: frontend / **path**: `web/`, `crates/app-platform`
- **依存**: 9.7, 9.8, 6.6
- **仕様**:
  - アプリのフロントバンドルを ObjectStore から **別オリジン or sandbox属性iframe**で配信。ホストのDOM/Cookie/storageに不可達。
  - CSP `connect-src` をゲートウェイに限定（任意URL fetch 不可）。ブラウザで PKCE トークンを取得しゲートウェイを直接叩く。
  - 宣言的UI（A/6.6レンダラ）とコードUI（B1）を同じシェルから起動できる。
- **受け入れ条件**:
  - [ ] B1 アプリがホスト権限/Cookieに触れずゲートウェイ経由でのみ通信する
  - [ ] CSP によりゲートウェイ以外への通信が遮断される
  - [ ] A（宣言的）と B1（コード）を同一シェルから一覧・起動できる

### Task 9.12: B2ランタイム（サンドボックス上アプリ関数実行＋egress allowlist）
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`, `crates/app-platform`
- **依存**: 9.7, 4.1
- **仕様**:
  - アプリのサーバ側関数を既存サンドボックス（Firecracker/gVisor）で**関数型実行**（リクエスト/eventで起動・破棄）。
    confidential client secret はサンドボックス内に隔離。**egressデフォルト遮断＋allowlist**（ゲートウェイ＋宣言allowlistのみ）。
  - cron/webhook を events から駆動。新規ホスティング基盤は作らず orchestrator を再利用。
- **受け入れ条件**:
  - [ ] B2 関数がサンドボックス内で起動・実行・破棄され、secret が外部に漏れない
  - [ ] egress が default-deny で、宣言 allowlist 以外への外部通信が遮断される
  - [ ] event/cron トリガで関数が起動し、ゲートウェイ経由で能力を呼べる

### Task 9.13: 配布（レジストリ／同意インストール／所有テーブルプロビジョン／信頼ティア／署名）
- **area**: infra / **path**: `crates/app-platform`, `deploy/`
- **依存**: 9.1, 9.7
- **仕様**:
  - 内部**レジストリ**へ不変 publish。**インストール** = 管理者が要求スコープに同意 → 所有テーブル自動プロビジョン＋ReBAC付与 →
    OAuthクライアント登録（9.7）。アンインストールで所有リソースを安全に撤去。
  - **信頼ティア**: first-party（署名・既定信頼）／in-house（管理者同意）／（将来）marketplace（審査）。
  - **オンプレ/エアギャップ**: 署名付きバンドルをネット不要でインポート（署名検証のみ）。
- **受け入れ条件**:
  - [ ] アプリを publish→同意インストールでき、所有テーブルが自動プロビジョンされる
  - [ ] 信頼ティアに応じてスコープ承認の要件が変わる（first-party は事前信頼）
  - [ ] オフライン環境で署名バンドルを検証してインストールできる

### Task 9.14: ミニアプリ SDK＋CLI（shiki app init/dev/publish）
- **area**: frontend / **path**: `sdk/`
- **依存**: 9.8, 9.13
- **仕様**:
  - 公開APIゲートウェイの能力面を、既存の OpenAPI/ts-rs 生成物から **SDK（型付きクライアント）**として配布（手書き型なし）。
  - **CLI**: `shiki app init`（雛形＋マニフェスト）／`dev`（ローカルアプリを dev ゲートウェイへ）／`publish`（package＋署名＋レジストリ登録）。
- **受け入れ条件**:
  - [ ] SDK 経由で能力API を型付きで呼べ、サーバ型と一致する
  - [ ] `shiki app init/dev/publish` で雛形作成→ローカル開発→publish が一気通貫で動く
  - [ ] 「我々（shikiチーム）が実装→簡単デプロイ」がCLIのみで完結する

### Task 9.15: ミニアプリ基盤の監査計装
- **area**: obs / **path**: `crates/app-gateway`, `crates/data`, `crates/app-platform`
- **依存**: 9.6, 9.10, 6.12
- **仕様**:
  - ゲートウェイ認可判定（スコープ∩ReBAC・許可/拒否）、行レベルauthz適用、FSM遷移、AI呼び出し（モデル/トークン/コスト）、
    インストール/同意/プロビジョンを **6.12/3.8 の監査・trace_id 枠**へ。スコープ拒否・authz拒否も記録。
- **受け入れ条件**:
  - [ ] 「誰が・どのアプリで・どの権限で・どの能力/データ/AIを呼んだか」が同一 trace_id で辿れる
  - [ ] スコープ拒否・行authz拒否・遷移拒否がセキュリティ事象として残る
  - [ ] (ユーザー×アプリ) のLLM/能力利用量が集計でき請求/クォータに供給される
