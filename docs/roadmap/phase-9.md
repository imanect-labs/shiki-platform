# Phase 9 — ミニアプリ／業務アプリ基盤

> **ステータス（2026-07-11・2026-10-17 改訂）: 9.1〜9.15 の主要部分を実装済み・未達あり（下記「実装突合」）。**
> 12 本の stacked PR で実装。
> data 基盤/行 authz/クエリ・マスク・集計抑制/FSM ガード/マニフェスト＋レジストリ/公開 API ゲートウェイ
> ＋OAuth2(PKCE)/token-exchange/能力アダプタ/ミニアプリ内 AI/同意インストール＋プロビジョン＋署名/
> B1 ランタイム（opaque origin＋CSP＋ホスト支援 PKCE）/B2 ランタイム（wasm 関数＋egress allowlist＋
> event/cron）/SDK＋CLI＋監査計装（利用量 API）。各段には crates/data・app-gateway・app-platform の
> IT（data_it/policy_threat_it/query_view_it/fsm_it/gateway_it/capability_it/ai_it/functions_it/b1_it/install_it）と
> web/e2e（miniapp-b1.spec）がある。ただし **DoD を一気通貫で通すテスト（publish→インストール→B1 PKCE→
> ゲートウェイ→data→遷移→AI）は無く**、実 Keycloak での PKCE・B1 隔離の実挙動も未検証（#683）。
> （旧: 「実装完了（9.1〜9.15）」「DoD の各段は … で検証済み」→ 2026-10-17 の実装突合で未達を確認し改訂）

> **実装突合（2026-10-17・main 329aba0）**: 行レベル認可の核（9.3/9.4: WHERE 強制合成・集計抑制・フィールドマスク・
> lookup 透過）は実装と CI で走る結合テストまで揃っている。一方、監査の trace_id 貫通・自動化 identity・FSM ガード・
> codegen SDK・ゲートウェイのレート制限などに未達がある。残り: create 時に任意 status を置け承認フローを迂回できる #629／
> ゲートウェイ全経路で trace_id が None 固定 #673（rag 経路の監査にアプリが残らない #620）／行述語拒否の監査が無い #674／
> 自動化（event/cron）サービス identity の経路が成立していない可能性（要実機確認）#675／FSM 付きアプリの配布経路が無い #676／
> ゲートウェイの (ユーザー×アプリ) レート制限・クォータ・破壊系の明示許可が無い #677／notify.send の受け手・マイタスクが無い #678／
> storage.write にアップロードが無い #679／agent.invoke に構造化データ参照ツールが無い #680／SDK の型が手書き・
> app-gateway に OpenAPI が無い・SDK が CI 対象外 #681／CLI dev に dev ゲートウェイが無く認証が手動 Cookie #682／
> テスト欠落（B1 PKCE 実 Keycloak・隔離・リビジョンの述語・DoD 一気通貫）#683／マニフェストの workflows・server.events
> 未検証・未使用 #684／遷移 outbox を消費する workflow-engine の data トリガ・data ノードが無い（Phase 10）#688・#687／
> web の手書き型重複（miniapp-b1-api.ts 等）#631／B2 の wasm 実行系に関わる wasmtime advisory で cargo-deny が赤 #580／
> app_platform_http IT が DB 再利用で 409 #365。

> 目的: Phase 6（A=宣言的ミニアプリ）の上に **B=コードベース・ミニアプリ**と**業務アプリ中核（構造化データ＋
> ワークフロー）**を足し、「社内業務アプリが自然増殖する基盤」を立ち上げる。中核は **out-of-trust な
> ミニアプリから内部APIをセキュアに叩く仕組み** ＝ ①公開APIゲートウェイ（唯一の入口・能力面再公開）
> ②ユーザー委譲OAuth2(PKCE)＋Keycloak再利用 ③二重ゲート（スコープ ∩ ユーザーReBAC）。
> 汎用PaaS/DBaaSは作らず「管理データサービス＋既存サンドボックス再利用＋公開API」の3点で構成する。
> 完了の定義(DoD): shikiチーム/パワーユーザーが、構造化データ（テーブル/フィールド/一覧/細粒度権限）と
> ワークフロー（承認フロー）を持つコードベース・ミニアプリをマニフェストで定義し、レジストリにpublish→
> 管理者が同意してインストール→所有テーブルが自動プロビジョンされロールで実行できる
> （実装ではインストール権限 = mini_app_code アーティファクトの owner・human 確定判断。Task 9.13 参照）。ミニアプリは別オリジン/CSP
> （B1）またはサンドボックス（B2）で隔離実行され、shiki機能（storage/data/rag/ai/identity/events）は
> 公開APIゲートウェイ＋スコープ付きトークン経由でのみ呼べ、毎呼び出しが `スコープ ∩ ユーザーReBAC` で認可され監査される。
> ミニアプリ内から `agent.invoke` でAI（ツール＋RAG）を呼べ、RAGは個人ReBACで再チェックされる。
>
> ⚠️ **行レベル認可（9.3-9.4）着手前に [設計上の落とし穴](../design-caveats.md) の PIT-17〜21 を解決すること。**
> 「行を見せない」を実装しても、集計の数値（PIT-17）・別システムにある述語の材料（PIT-18）・マスク列での
> 並べ替え/絞り込み（PIT-19）・lookup のテーブル間参照（PIT-20）・`WHERE` 強制注入だけでは塞げない漏れ口（PIT-21）
> という隙間から漏れる。基盤全体で最も正しさがクリティカルな領域。

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
    実装の照合対象は `requested_scopes`（`authz::CapabilityScope`）と `tools`（`agent_core::ToolName`）。
    マニフェストの `workflows`（参照 ID）と `server.events` は検証もインストールもされない → #684。
- **受け入れ条件**:
  - [x] マニフェスト付き mini_app_code アーティファクトを作成・新バージョン追記でき、過去バージョンが不変で取れる
  - [x] マニフェストのスキーマ検証（要求スコープ・所有テーブル・エントリ）が効き、不正は拒否される
  - [ ] 存在しないスコープ/ツール/アクションIDを参照するマニフェストが語彙照合で拒否される（スコープ・ツールは拒否を検証済み。workflows・server.events は未照合 → #684）
  - [x] A（宣言的）と B（コード）が同じ共有・バージョン・監査経路に乗る

### Task 9.2: 構造化データサービス（record／スキーマレジストリ＋式インデックス）
- **area**: data / **path**: `crates/data`, migrations
- **依存**: 6.1
- **仕様**:
  - 既存Postgres上の管理サービス。`record(id, table_id, org, data JSONB, rev, owner, created_at, updated_at)` ＋
    `table_schema(id, app_id, fields[], validations)`。**ランタイムDDLを打たない**（行は共有テーブルにJSONBで格納）。
  - フィールド型: text/number/date/datetime/select/multi-select/**user参照/dept参照/file参照(storage)/record参照/lookup/計算**。
    （実装の型名: `user_ref`／**dept参照は `role_ref`（部署はロールとして表す）**／`file_ref`／`record_ref`／`lookup`／`computed`・`crates/data/src/model.rs`）
  - スキーマで宣言されたフィルタ/ソート対象フィールドに **JSONB式インデックス**を生成。書込時にサーバ検証（型・必須・unique・参照整合）。
  - テーブル＝OpenFGA `data_table` 型（viewer/editor/owner）。Q6の第1層ReBAC。生DBはアプリに一切公開しない。
- **受け入れ条件**:
  - [x] スキーマを定義してテーブルを作成し、型検証付きでレコードCRUDできる
  - [x] 宣言フィールドに式インデックスが張られフィルタ/ソートが効く
  - [x] user/dept/file/record 参照型が解決され整合性検証される

### Task 9.3: 行レベル認可述語エンジン（WHERE強制注入・クエリコンパイラ・集計適用）
- **area**: data / **path**: `crates/data`, `crates/authz`
- **依存**: 9.2, 6.1
- **仕様**:
  - テーブル定義の宣言的 `row_policy`（read/write）を、
    クエリ時に**省略不可・上書き不可のWHERE述語**へコンパイルしANDで強制付与。クライアントは生SQLを送れない。
    述語の語彙は閉じた文法（`crates/data/src/policy/ast.rs`）: `any`/`all`・`field_cmp`（`eq`/`ne`/`in`、オペランドは
    リテラル・`user_id`＝`$user.id`・`user_roles{subtree}`＝所属ロール集合）・`has_role{role,subtree}`・`is_owner`・`public`。
    **部署はロールとして表し**（`role_ref`）、配下部署は `subtree=true`（OpenFGA の member 展開）で表す。`not` は持たない。
    （旧: `$user.id/$user.dept(subtree)/$user.role/$user.groups` を参照可 → 置換: 上記語彙。dept は role に統合、
    `$user.groups` に相当する演算子は無い）
  - 述語は**集計（count/sum/avg）にも適用**し、非可読行が件数/合計から漏れない（permission-aware と同一保証）。
  - 述語キー（owner/dept/status等）は式インデックス前提。**個別共有はスパースに OpenFGA tuple へ逃がし** `OR id IN (共有id)` を付与。
- **受け入れ条件**:
  - [x] row_policy で「自分/自ロール/公開」等の行だけが返り、他は取得・集計の双方で除外される
  - [x] クライアント指定フィルタに何を渡してもauthz述語はバイパスできない
  - [x] 個別共有した特定レコードだけ追加で見え、tuple数が共有件数に比例（全件には載らない）

### Task 9.4: 宣言的クエリ／保存ビュー／集計＋フィールドマスク
- **area**: data / **path**: `crates/data`, `crates/app-platform`
- **依存**: 9.3
- **仕様**:
  - 宣言的クエリAPI（filter/sort/page/aggregate）。**生SQL非公開**。クエリは 9.3 の述語と必ず合成して実行。
  - **保存ビュー**（一覧/グラフ/カレンダー相当）を artifact 化（6.1枠・ReBAC共有・バージョン）。
  - **フィールドマスク**（field_policy）: 行が見えても特定フィールドをロールで非可視化（取得後マスク）。
- **受け入れ条件**:
  - [x] filter/sort/page/aggregate がauthz述語と合成されて正しい結果を返す
  - [ ] 保存ビューを作成・共有・バージョン切替でき、実行時に述語が効く（作成・共有・閲覧者ごとの述語再評価は検証済み。バージョン切替は実装済み・検証テスト欠落 #724）
  - [x] field_policy 対象フィールドが無権限ロールには返らない

### Task 9.5: レコード・リビジョン履歴＋楽観ロック
- **area**: data / **path**: `crates/data`, migrations
- **依存**: 9.2
- **仕様**:
  - レコード更新を**追記型 changelog**（誰が・いつ・どのフィールドをどう変えたか）で記録。過去リビジョン取得可。
  - `rev` による楽観ロック（同時更新の衝突検出→409）。
- **受け入れ条件**:
  - [x] 更新ごとにリビジョンが残り、フィールド単位の差分を辿れる
  - [x] 競合する同時更新が検出され安全に拒否される
  - [ ] 履歴取得が 9.3 の認可（行/フィールド）に従う（実装済み・検証テスト欠落 #683）

### Task 9.6: 公開APIゲートウェイ(BFF) — 能力面・スコープ検証・per-call OpenFGA（二重ゲート）
- **area**: api / **path**: `crates/app-gateway`
- **依存**: 9.2, 3.9
- **仕様**:
  - 内部APIを直接公開せず、**唯一の入口**としてキュレーション済み・バージョン付き・スコープ付きの能力面を再公開。
  - 各リクエストで: ①アクセストークン検証 → ②要求スコープがアプリ付与スコープ＋リソース束縛内か確認 →
    ③**呼出ユーザーのReBACを OpenFGA で per-resource 判定**。**実効 = スコープ ∩ ユーザーReBAC**（二重ゲート）。
  - レート制限/クォータ（(ユーザー×アプリ)）、破壊系/高コスト系は 3.9 の明示許可ポリシを継承。
    （実装済みの上限は AI の日次予算（429）のみ。能力全般のレート制限・クォータと破壊系の明示許可は未実装 → #677）
- **受け入れ条件**:
  - [ ] 内部エンドポイントへゲートウェイを介さず到達できない（構造的に担保: ゲートウェイは別リスナ・Bearer 専用、内部 API は Cookie セッションのみで Bearer 入口なし。到達不能を確かめるテストは欠落 #683）
  - [x] アプリが広いスコープでも、ユーザー非可読リソースには到達できない（二重ゲート）
  - [ ] スコープ外/未宣言リソースへのアクセスが拒否され監査に残る（拒否は検証済み。スコープ拒否の deny 監査は実装済みだが検証テスト欠落 #683）

### Task 9.7: OAuth2クライアント登録＆PKCE／token-exchange（Keycloak連携）
- **area**: auth / **path**: `crates/app-gateway`, `crates/authz`
- **依存**: 9.6
- **仕様**:
  - 各ミニアプリ = Keycloak の OAuth2 クライアント（レジストリ登録と連動）。**新規認証基盤は作らない**。
  - **B1=public client**（authcode+PKCE・secretなし・短命トークン）。**B2=confidential client**（ユーザー操作は
    **token-exchange / on-behalf-of** でユーザー代理を維持）。
    B2 の client secret は**ホスト側が保持**する（`crates/secrets`・宛先束縛=token endpoint・解決主体はアプリの service identity）。
    ホストが token-exchange 済みの Bearer を関数のゲートウェイ呼び出しにだけ付与し、ゲスト（wasm）は secret もトークンも観測できない
    （`crates/api/src/gateway_functions.rs`・`crates/app-platform/src/functions.rs`）。
    （旧: secret はサンドボックス内 → 置換: ホスト保持＋Bearer 付与。ゲストに資格情報を持たせない INV-1 に合わせた）
  - **自動化のみ**: 所有データ(`data.app:*`)限定の狭スコープ・サービスidentityを発行（ReBACで所有データに束縛）。
    （インストール時の ReBAC 付与は `owner@miniapp:<app_id>`。一方ゲートウェイは client_credentials トークンの sub も
    `PrincipalKind::User` として扱うため、自動化経路が所有データへ届かない可能性がある・要実機確認 → #675）
- **受け入れ条件**:
  - [ ] B1 が PKCE でトークンを取得しゲートウェイを叩ける（client secret 不要）（実装済み: ホスト支援 PKCE・public+S256 client。実 Keycloak での検証テスト欠落 #683）
  - [x] B2 が token-exchange で「ユーザーの代理」として認可される（アプリ単独権限に昇格しない）
  - [ ] 自動化サービスidentityが所有データのみに限定され、越境が拒否される → #675

### Task 9.8: 能力アダプタ（storage/data/rag/identity/events を能力面に薄く公開）
- **area**: api / **path**: `crates/app-gateway`, `crates/api`
- **依存**: 9.6
- **仕様**:
  - 能力カタログを `<能力>.<操作>` で実装: `storage.read/write`・`data.read/write/schema`・`rag.query`・
    `identity.read`・`events.subscribe`/`notify.send`。各操作はリソース束縛と per-call OpenFGA に従う。
    （実装の現状: `storage.write` はフォルダ作成のみでアップロードは無い → #679。`notify.send` は台帳記録＋監査のみで、
    受け手の通知一覧 API・UI が無い → #678）
  - `rag.query` は permission-aware（個人ReBAC再チェック）。`identity.read` は最小限（id/所属ロール）。
- **受け入れ条件**:
  - [x] 各能力が宣言スコープ＋ReBACの範囲でのみ動作する
  - [ ] `rag.query` がアプリ経由でもユーザー非可読文書を返さない（実装済み: 呼出ユーザーの AuthContext で SearchService の二段 authz へ委譲。ゲートウェイ IT は RagPort スタブで、アプリ経由の検証テスト欠落 #683）
  - [x] 能力ごとに利用量・コストが (ユーザー×アプリ) で計上される

### Task 9.9: ミニアプリ内AI（llm.invoke／agent.invoke＋コスト計上＋ガードレール）
- **area**: ai / **path**: `crates/app-gateway`, `crates/agent-core`, `crates/llm-gateway`
- **依存**: 9.6, 6.9, 5.1
- **仕様**:
  - **raw `llm.invoke`**（アプリがプロンプト供給）＋ **`agent.invoke`**（agent-core起動・ツール実行＋RAG＋構造化データ参照）。
  - `agent.invoke` のツール = **アプリ宣言ツール ∩ template ∩ アプリに効いている能力スコープ ∩ ユーザーReBAC**
    （読み取り系ツールは同じものを読む API と同じスコープを要する: `doc_search` = `rag.query`・`doc_read` = `storage.read`）。コンテキストにアプリデータ/RAGスコープを渡せるが
    **RAGは個人ReBACで再チェック**。モデル/トークン予算ガードレール（アプリ登録時宣言＋管理者キャップ）。
    （実装の現状: ゲートウェイ文脈で提示できるツールは `doc_search`・`doc_read` のみで、構造化データ参照ツールは無い → #680）
  - コストは (ユーザー×アプリ) で計上、llm-gateway 経由で Langfuse に `app_id+user+trace_id`。SSEストリーミングを通す。
    （ゲートウェイ経路では trace_id が `None` 固定 → #673）
- **受け入れ条件**:
  - [x] ミニアプリ内から llm.invoke / agent.invoke が呼べ、結果がストリーミングされる
  - [ ] agent.invoke のツール/RAGがユーザーReBACで絞られ、非可読データが混入しない（ツール提示＝宣言 ∩ 閉集合 ∩ 有効スコープは単体検証済み。RAG の個人 ReBAC 再チェックは SearchService 委譲で、アプリ経由の検証テスト欠落 #683）
  - [x] モデル/予算上限が効き、利用量が (ユーザー×アプリ) で記録される

### Task 9.10: FSM ガード（status 遷移の宣言的ガード）※2026-07 改訂
> 📝 **改訂（#97・[miniapp-platform.md](../miniapp-platform.md) §1）**: 旧「軽量FSMエンジン」は**廃止**。
> FSM は data サービスの**宣言的ガード**（データの不変条件）に縮退し、**副作用・プロセス実行はすべて
> Phase 10 の workflow-engine へ委譲**する（遷移コミット→outbox イベント→トリガ）。
- **area**: data / **path**: `crates/data`
- **依存**: 9.3, 6.5
- **仕様**:
  - テーブルに紐づく宣言的 states/transitions を artifact 化。状態 = レコードの `status` フィールド。
    **遷移は record 書込と同一トランザクション**（原子的・途中状態なし）。
  - **遷移認可 = 9.3 の述語エンジンを再利用**（`actor: field:承認者 || role:部長` 等）。サーバ強制で定義外ジャンプ不可。
  - **副作用はこのタスクに含めない**: 遷移コミットで outbox イベントを発行するのみ（通知・転記・AI 実行は
    workflow-engine 側のトリガ＝Phase 10.3）。ワークフローが status を進める場合も必ずこの遷移 API を叩く
    （ガード・認可を再評価。エンジンに直接書かせない）。
    （実装の現状: outbox `data.record.transitioned` の消費者は B2 の event トリガと `events.subscribe` のみ。workflow relay は
    読み飛ばしており、data イベントのトリガ（10.3 Stage B）→ #688、遷移を叩く data ノード（10.6b）→ #687 が未実装）
  - 担当者は user/dept/role/フィールド動的。保留中タスクを events 経由で「マイタスク」に。（マイタスクは未実装 → #678）
  - FSM 定義はマニフェストに同梱されず、テーブルスキーマの `fsm_ref` で既存 artifact を参照するのみ。インストールで FSM は
    プロビジョンされず、アプリ利用者への FSM artifact の閲覧権付与も無い（遷移は呼出ユーザーの権限で FSM を読む）→ #676
- **受け入れ条件**:
  - [ ] 承認フロー（申請→承認→完了/差戻し）が定義どおり遷移し、定義外遷移が拒否される（遷移 API では検証済み。create 時に任意の status を置けるため定義外の状態に入れる → #629）
  - [ ] 遷移認可が行述語で評価され、権限のない人は遷移できない（workflow-engine 経由でも同様）（data サービスの遷移 API では検証済み。workflow-engine から遷移を叩く経路が無い → #687）
  - [ ] 全遷移がリビジョン履歴＋監査に残り、status が行可視性を駆動し、outbox イベントが発行される（リビジョン・監査・outbox は検証済み。status は row_policy の `field_cmp` で参照できるが、status による可視性変化の検証テスト欠落 #683）

### Task 9.11: B1ランタイム（別オリジン配信＋CSP＋ブラウザOAuth＋レンダラ統合）
- **area**: frontend / **path**: `web/`, `crates/app-platform`
- **依存**: 9.7, 9.8, 6.6
- **仕様**:
  - アプリのフロントバンドルを ObjectStore から **別オリジン or sandbox属性iframe**で配信。ホストのDOM/Cookie/storageに不可達。
  - CSP `connect-src` をゲートウェイに限定（任意URL fetch 不可）。ブラウザで PKCE トークンを取得しゲートウェイを直接叩く。
    （実装: opaque origin の iframe は自力で OAuth リダイレクトを完結できないため、**ホスト支援 PKCE**＝ホストシェルが popup で
    authorization code を取得・token 交換し、アクセストークンだけを postMessage で渡す。code_verifier はシェルのメモリのみ・
    `web/src/lib/miniapp-oauth.ts`）
  - 宣言的UI（A/6.6レンダラ）とコードUI（B1）を同じシェルから起動できる。
- **受け入れ条件**:
  - [ ] B1 アプリがホスト権限/Cookieに触れずゲートウェイ経由でのみ通信する（実装済み: sandbox 属性（allow-same-origin なし）・Cookie 非発行をヘッダで検証。実ブラウザでの隔離挙動の検証テスト欠落 #683）
  - [ ] CSP によりゲートウェイ以外への通信が遮断される（実装済み: CSP `connect-src` の内容は b1_it・e2e で検証。実ブラウザでの遮断挙動の検証テスト欠落 #683）
  - [x] A（宣言的）と B1（コード）を同一シェルから一覧・起動できる

### Task 9.12: B2ランタイム（サンドボックス上アプリ関数実行＋egress allowlist）
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`, `crates/app-platform`
- **依存**: 9.7, 4.1
- **仕様**:
  - アプリのサーバ側関数を既存サンドボックス（**アルファ=wasm ティア（4.12）**。Firecracker/gVisor はポストアルファの
    昇格先・design §4.6）で**関数型実行**（リクエスト/eventで起動・破棄）。
    confidential client secret はホスト側に保持し、ゲストには渡さない（Task 9.7 の注記参照）。
    （旧: secret はサンドボックス内に隔離 → 置換: ホスト保持＋Bearer 付与・`crates/app-platform/src/functions.rs`）
    **egressデフォルト遮断＋allowlist**（ゲートウェイ＋宣言allowlistのみ）。
  - cron/webhook を events から駆動。新規ホスティング基盤は作らず orchestrator を再利用。
    （実装は event（outbox 台帳 `miniapp-functions`）と cron（`app_function_schedule`）の 2 種・`crates/api/src/miniapp_triggers.rs`。
    webhook 起動の実装は無い）
- **受け入れ条件**:
  - [x] B2 関数がサンドボックス内で起動・実行・破棄され、secret が外部に漏れない
  - [x] egress が default-deny で、宣言 allowlist 以外への外部通信が遮断される
  - [ ] event/cron トリガで関数が起動し、ゲートウェイ経由で能力を呼べる（起動と実行台帳は検証済み。service identity でのゲートウェイ能力呼び出しが成立しない可能性・要実機確認 → #675）

### Task 9.13: 配布（レジストリ／同意インストール／所有テーブルプロビジョン／信頼ティア／署名）
- **area**: infra / **path**: `crates/app-platform`, `deploy/`
- **依存**: 9.1, 9.7
- **仕様**:
  - 内部**レジストリ**へ不変 publish。**インストール** = 管理者が要求スコープに同意 → 所有テーブル自動プロビジョン＋ReBAC付与 →
    OAuthクライアント登録（9.7）。アンインストールで所有リソースを安全に撤去。
    （実装: **インストール権限 = mini_app_code アーティファクトの owner ReBAC**（human 確定判断・miniapp 型に installer relation は
    作らない・`crates/app-platform/src/install.rs`）。owner が granted_scopes（requested の部分集合）を指定して同意し、プロビジョンした
    テーブルへ `owner@miniapp` と指定ロールの viewer/editor を付与する。旧: 「管理者が同意」）
  - **信頼ティア**: first-party（署名・既定信頼）／in-house（管理者同意）／（将来）marketplace（審査）。
    （実装差: first-party は**信頼鍵で検証できる署名を必須にする点だけ**が in-house と異なり、スコープ同意（granted_scopes の指定）は
    同じ。「事前信頼」による同意の省略は無い。in-house は owner の同意のみ。marketplace・未知ティアは拒否）
  - **オンプレ/エアギャップ**: 署名付きバンドルをネット不要でインポート（署名検証のみ）。
- **受け入れ条件**:
  - [x] アプリを publish→同意インストールでき、所有テーブルが自動プロビジョンされる（HTTP 経由の IT は DB 再利用で 409 になる問題あり #365）
  - [ ] 信頼ティアに応じてスコープ承認の要件が変わる（first-party は事前信頼）（実装は first-party に署名必須を足すのみで、スコープ同意は同一。上記「実装差」参照・意図か未達かは未確認）
  - [x] オフライン環境で署名バンドルを検証してインストールできる

### Task 9.14: ミニアプリ SDK＋CLI（shiki app init/dev/publish）
- **area**: frontend / **path**: `sdk/`
- **依存**: 9.8, 9.13
- **仕様**:
  - 公開APIゲートウェイの能力面を、既存の OpenAPI/ts-rs 生成物から **SDK（型付きクライアント）**として配布（手書き型なし）。
  - **CLI**: `shiki app init`（雛形＋マニフェスト）／`dev`（ローカルアプリを dev ゲートウェイへ）／`publish`（package＋署名＋レジストリ登録）。
    （実装の現状: `sdk/app-sdk` の型は手書きで、app-gateway に OpenAPI（utoipa）が無く生成元が無い。SDK は data・whoami・identity・
    rag・notify のみで storage・events・ai・functions を持たず、`sdk/` は CI 対象外 → #681。CLI の `dev` は BFF へバンドルを
    upload するだけで dev ゲートウェイは無く、認証はブラウザのセッション Cookie を `SHIKI_COOKIE` に写す方式 → #682）
- **受け入れ条件**:
  - [ ] SDK 経由で能力API を型付きで呼べ、サーバ型と一致する → #681
  - [ ] `shiki app init/dev/publish` で雛形作成→ローカル開発→publish が一気通貫で動く（init/dev/publish は実装済み。dev ゲートウェイが無く、一気通貫の検証テストも無い → #682）
  - [ ] 「我々（shikiチーム）が実装→簡単デプロイ」がCLIのみで完結する（認証に手動 Cookie が要る → #682）

### Task 9.15: ミニアプリ基盤の監査計装
- **area**: obs / **path**: `crates/app-gateway`, `crates/data`, `crates/app-platform`
- **依存**: 9.6, 9.10, 6.12
- **仕様**:
  - ゲートウェイ認可判定（スコープ∩ReBAC・許可/拒否）、行レベルauthz適用、FSM遷移、AI呼び出し（モデル/トークン/コスト）、
    インストール/同意/プロビジョンを **6.12/3.8 の監査・trace_id 枠**へ。スコープ拒否・authz拒否も記録。
- **受け入れ条件**:
  - [ ] 「誰が・どのアプリで・どの権限で・どの能力/データ/AIを呼んだか」が同一 trace_id で辿れる → #673（ゲートウェイ全経路で trace_id が None 固定。rag 経路の監査にアプリが残らない件は #620）
  - [ ] スコープ拒否・行authz拒否・遷移拒否がセキュリティ事象として残る（スコープ拒否・遷移拒否・テーブル単位の ReBAC 拒否は記録あり。行述語による拒否は記録されない → #674）
  - [ ] (ユーザー×アプリ) のLLM/能力利用量が集計でき請求/クォータに供給される（集計 API `/apps/{app_id}/usage` は実装・検証済み。能力利用量をクォータ強制へ供給する経路は無い → #677）
