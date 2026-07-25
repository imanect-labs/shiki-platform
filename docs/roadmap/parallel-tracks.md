# 並行 / 将来トラック

> フェーズの直線（Phase 0→8）に乗らない作業を、トラックごとにまとめる。
> 各トラックは独立した起動タイミングを持ち、対応する基盤フェーズが安定してから着手する。
> タスク粒度は各 Phase ファイルと同じ**イシュー粒度**（1タスク=1 GitHub Issue）。
> task ID はトラック接頭辞（SK / V2 / SAAS / GUI2 / BR）を使う。
> 関連: [要件定義書](../requirements.md)（6. スコープ外/将来, FR-1 skillex統合）/ [設計書](../design.md) / [ROADMAP](../roadmap.md)

---

## トラックWF — ワークフロー基盤エンジン核心の前倒し（task ID は 10.x のまま）

> **2026-07-07 決定（#121）**: Phase 10 の **Stage A（エンジン核心）を Phase 5〜9 と並走で前倒し**する。
> 本トラックは原則として新規タスクを定義せず——**タスク定義・Stage 分割（A/B）・実行順・DoD の正本は
> [phase-10.md](./phase-10.md) 冒頭の「部分前倒し」節**であり、task ID は 10.x（＋前提の 6.1）をそのまま使う
> （接頭辞トラックの例外。同一タスクに二重 ID を与えない）。
> **唯一の新規タスクは `P10-A0`（outbox の per-consumer fan-out 化・10.3 の前提）**で、これも定義の正本は
> phase-10.md（Task P10-A0）に置く（この節ではタスクを定義しない方針の明示的な例外）。
> 詳細設計は [docs/workflow/](../workflow/README.md)（#119）が正本。

- **Stage A 対象**: P10-A0（outbox fan-out・新設）・10.0（durable 切り出し・新設）・10.1a・10.2・10.3・10.4a・10.5・10.6a・10.7・10.8・10.9・10.10。
  前提は **6.1（artifact 共通枠）の先行実施＋P10-A0（outbox の per-consumer fan-out 化）**。
- **Stage B（Phase 6/9 合流後）**: 10.1b・10.4b・10.6b・10.11〜10.15（skill・dnd・AI 編集・実行履歴 UI・data 系ノード）。

---

## トラックSK — SaaS共有コントロールプレーン＆skillex統合（接頭辞 SK.x）

> **重要**: 統一は **SaaS版限定**。オンプレ版は shiki・skillex とも認証基盤を切り離し単独運用（本トラック対象外）。
> 詳細境界は `docs/requirements.md` FR-1.1 / `docs/design.md` 4.1.1 を参照。

- **目的**: SaaS版で、shiki と skillex を束ねる**マルチテナントな共有コントロールプレーン**
  （Keycloak＝統一User／Org・メンバー招待・サービスアクセス権／統一請求／管理ダッシュボード）を、
  **shiki repo 所有の SaaS専用モジュール**として構築する。
  **3層境界**: ①User=統一 ②サービスへの入場券＋管理者バッジ=統一 ③サービス内の細かいロール/ReBAC/設定=分離。
  **請求=統一（Org単位1請求・サービス別内訳）／利用量=分離（集約値のみ受領・クォータ強制は各サービス）**。
- **契約の正本**: skillex は**別リポ**。参照する契約（OIDC設定・サービスアクセス権API・利用量集約イベント・
  トークン aud/scope）の正本は **shiki repo `contracts/`** に置き公開する（SK.1/SK.4 で具体化、バージョン管理）。
- **タイミング**: **Phase 0 の認証が安定したら並行**。skillex は並行進行中のため**ブロックしないこと**を最優先。
  Phase 0 で AuthN 向き先の設定差し替え（共有issuer ⇔ ローカルKeycloak）の継ぎ目を織り込む。

### タスク一覧

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| SK.1 | 共有realm設計＋skillex client/scope/audience 定義 | auth | 0.2 |
| SK.2 | skillex DLC/LLM 用トークン発行（client credentials / token exchange） | auth | SK.1 |
| SK.3 | サービスアクセス権モデル（統一）×サービス内認可（分離）の境界実装 | auth | SK.1 |
| SK.4 | skillex web app 向け認証認可エンドポイント連携 | auth | SK.2, SK.3 |
| SK.5 | skillex トークンの監査・失効・ローテーション | auth | SK.2 |
| SK.6 | 共有コントロールプレーン：Org・メンバー招待・サービスアクセス権＋管理ダッシュボード | auth | SK.3 |
| SK.7 | 統一請求：Org単位1請求・サービス別内訳＋利用量集約イベント受信 | infra | SK.6 |

---

### Task SK.1: 共有realm設計＋skillex client/scope/audience 定義
- **area**: auth / **path**: `deploy/keycloak/`, `docs/design.md`
- **依存**: 0.2（Keycloak/OIDCログイン配線）
- **仕様**:
  - shiki の Keycloak realm を**共有アイデンティティプール**として設計し、skillex を同一 realm に
    フェデレートさせる構成を確定（別 realm 案との比較・決定根拠を記す）。
  - skillex 用の OIDC client、必要 scope、`audience`（DLC/LLM リソースサーバ向け）を定義。
  - skillex 側ユーザーと shiki ユーザーが**同一プール**で衝突しない命名/属性マッピングを決める。
- **受け入れ条件**:
  - [ ] skillex client が realm に登録され、想定 scope/audience でトークンを取得できる
  - [ ] 共有プール構成が design.md に図入りで記録される
  - [ ] shiki ユーザーと skillex ユーザーが同一プール上で一意に識別される

### Task SK.2: skillex DLC/LLM 用トークン発行
- **area**: auth / **path**: `deploy/keycloak/`, `crates/auth`
- **依存**: SK.1
- **仕様**:
  - skillex の **DLC/LLM 利用**に必要なアクセストークンを Keycloak が発行する。
    machine-to-machine は client credentials、ユーザー文脈が要る場合は token exchange を使う。
  - トークンに DLC/LLM リソース向けの `audience` と最小 scope を載せ、過剰権限を与えない。
  - skillex 側の検証手順（公開鍵/JWKS、`aud`/`iss` 検証）をドキュメント化する。
- **受け入れ条件**:
  - [ ] skillex が発行トークンで DLC/LLM エンドポイントにアクセスできる
  - [ ] トークンの audience/scope が想定リソースに限定される
  - [ ] skillex 側の検証手順が文書化され、サンプルで検証成功する

### Task SK.3: サービスアクセス権モデル（統一）×サービス内認可（分離）の境界実装
- **area**: auth / **path**: `crates/auth`, `crates/authz`, 共有コントロールプレーン
- **依存**: SK.1
- **仕様**:
  - **統一層**: 共有コントロールプレーンが `Org × Member × サービスアクセス権`（`shiki: なし/利用者/サービス管理者`,
    `skillex: なし/利用者/管理者` の粗い区分）を保持。User は同一プールで両サービス共通。
  - **分離層**: サービス内の細かい認可は各サービスが独立に保持。**shiki の ReBAC（OpenFGA）は shiki データプレーンに閉じる**。
    skillex も自前の認可を持つ。共有プレーンは「入場券＋管理者バッジ」までしか知らない。
  - skillex 由来ユーザーに shiki リソースへのアンビエント権限が漏れないよう、shiki の認可判定は
    `サービスアクセス権あり` を前提に shiki 自身の ReBAC で決定する（認可コンテキスト `principal + org` で評価）。
- **受け入れ条件**:
  - [ ] サービスアクセス権なしのユーザーは該当サービスに入れない
  - [ ] shiki 内の細かい認可が shiki の authz store のみで決まる（共有プレーンに依存しない）
  - [ ] 同一ユーザーが両サービスで同一 principal として扱われる

### Task SK.4: skillex web app 向け認証認可エンドポイント連携
- **area**: auth / **path**: `crates/api`, `deploy/keycloak/`
- **依存**: SK.2, SK.3
- **仕様**:
  - skillex の web app（訓練フィードバック閲覧・各種設定）が必要とする
    認証認可エンドポイント（OIDC ログイン/コールバック、トークン introspection、必要なら userinfo）
    との連携を確立する。
  - CORS / リダイレクト URI / セッション境界を skillex web app の origin 向けに設定する。
- **受け入れ条件**:
  - [ ] skillex web app から OIDC ログインが完走しトークンを取得できる
  - [ ] 訓練フィードバック閲覧・設定画面が認証済みで保護される
  - [ ] 許可された origin のみが認証フローを利用できる

### Task SK.5: skillex トークンの監査・失効・ローテーション
- **area**: auth / **path**: `crates/auth`, `deploy/keycloak/`
- **依存**: SK.2
- **仕様**:
  - skillex 向けに発行したトークンの発行/失効を監査ログに記録し、shiki の監査と突合可能にする。
  - client secret / 署名鍵のローテーション手順、失効（セッション/トークン無効化）を整備する。
- **受け入れ条件**:
  - [ ] skillex トークンの発行・失効が監査ログに残る
  - [ ] client secret/鍵のローテーション手順が文書化・実行できる
  - [ ] 失効後のトークンが DLC/LLM/web app で拒否される

### Task SK.6: 共有コントロールプレーン（Org・メンバー招待・サービスアクセス権＋管理ダッシュボード）
- **area**: auth / **path**: 共有コントロールプレーン（SaaS専用モジュール, shiki repo所有）, `web/`
- **依存**: SK.3
- **仕様**:
  - **SaaS専用モジュール**として、Organization・Membership・サービスアクセス権を保持するデータモデルとAPI。
    Keycloak（統一User）と連携し、メンバー招待（メール招待→Org参加）、サービスロール付与（粗い区分）を提供。
  - **統一「アカウント管理画面」（web）**: 統一シェル＋共有ページ（メンバー招待・サービスアクセス権付与・ロール/グループ・請求閲覧）。
    各サービスの設定ページは**マイクロフロントエンドで合成**（ページが分かれているだけの一体UI）。
    各ページは自サービスのAPI/ストアを叩き、**authz・設定データは分離**。合成の契約は `contracts/` に置く。
    各サービス管理ページは「シェル埋め込み／単独」両対応の自己完結モジュール（オンプレは単独管理画面として動作）。
  - shiki/skillex の各データプレーンはこのモジュールからサービスアクセス権を読む（OIDCクレーム or API）。
  - **オンプレ shiki はこのモジュールを積まない**（マルチテナント・外部依存のため SaaS限定）。
- **受け入れ条件**:
  - [ ] Org管理者がメンバーを招待し、サービスアクセス権を付与できる
  - [ ] 付与/剥奪が shiki・skillex の入場可否に即時反映される
  - [ ] 各サービス設定ページがシェル埋め込み／単独の両方で動く
  - [ ] このモジュールがオンプレ構成に含まれない（ビルド/デプロイで分離）

### Task SK.7: 統一請求（Org単位1請求・サービス別内訳＋利用量集約イベント受信）
- **area**: infra / **path**: 共有コントロールプレーン（請求）, `crates/llm-gateway`（計測連携）
- **依存**: SK.6
- **仕様**:
  - 各サービスが**自前で計測した集約使用量**（shiki=LLMトークン/コスト、skillex=DLC/LLM利用量）を、
    **集約イベントとして**共有請求プレーンへ送る（生ログは送らない＝利用量は分離保持）。
  - 共有請求プレーンが支払い方法・サブスク・**請求書生成**を担い、**Org単位で1請求・サービス別ライン内訳**を出す。
    プラン/サブスクはサービス別、束ねて1請求。**クォータ/上限の強制は各サービス側**、ここは金額集約のみ。
- **受け入れ条件**:
  - [ ] shiki/skillex の集約使用量が請求プレーンに届く
  - [ ] Org単位の1請求書にサービス別内訳が出る
  - [ ] 生の利用量ログが請求プレーンに渡らない（分離が保たれる）

---

## トラックV2 — 資料作成v2（ブラウザ内編集）（接頭辞 V2.x）

> 📝 **本トラックは [Phase 11](./phase-11.md) に昇格・統合（2026-07-05・#97）**。エディタは **Collabora Online に確定**
> （`OfficeSuite` トレイトで OnlyOffice への退路を確保・V2.1 の選定は完了扱い）。V2.2〜V2.5 は Phase 11 の
> Task 11.5〜11.8 が置き換える。以下は経緯記録として残す。

- **目的**: 資料作成 v1（ライブラリ生成）の上に、**ブラウザ内での Office 文書編集**を載せる。
  OnlyOffice Docs / Collabora Online を組み込み、AI 生成 → ユーザー手直し → 保存の一連フローを実現する。
  保存は StorageService 経由でバージョニングし、RAG へ再索引する。**自作エディタは最終手段**。
- **タイミング**: ~~Phase 7（資料作成 v1）の後~~ → **Phase 11 として実施**。

### タスク一覧

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| V2.1 | エディタ選定（OnlyOffice Docs / Collabora 比較・PoC） | docgen | – |
| V2.2 | エディタ iframe 組込＋文書ロード | docgen | V2.1 |
| V2.3 | 保存コールバック（WOPI類似）→ StorageService 保存 | docgen | V2.2 |
| V2.4 | 保存後の RAG 再索引トリガ | docgen | V2.3 |
| V2.5 | AI生成→手直し→保存の一連フロー結線 | docgen | V2.3 |

---

### Task V2.1: エディタ選定（OnlyOffice Docs / Collabora 比較・PoC）
- **area**: docgen / **path**: `docs/design.md`, `deploy/`
- **依存**: なし（Phase 7 完了後に開始）
- **仕様**:
  - OnlyOffice Docs と Collabora Online を、docx/pptx/xlsx の編集互換性・ライセンス・
    オンプレ/エアギャップ適合・保存コールバック方式（WOPI 互換性）で比較し PoC する。
  - **自作は最終手段**。既製品で要件を満たせるかを先に確定する。
- **受け入れ条件**:
  - [ ] 両候補で docx/pptx/xlsx を開いて編集できる PoC が動く
  - [ ] 採用候補と決定根拠（互換性・ライセンス・オンプレ適合）が文書化される
  - [ ] エアギャップ環境での自己ホスト可否が確認される

### Task V2.2: エディタ iframe 組込＋文書ロード
- **area**: docgen / **path**: `web/`, `crates/api`
- **依存**: V2.1
- **仕様**:
  - 選定エディタを **iframe** で web app に組み込み、StorageService 上の文書 node を開く。
  - エディタ起動トークン/権限を発行し、閲覧者の ReBAC 権限を反映して開く（編集権限のチェック）。
- **受け入れ条件**:
  - [ ] StorageService 上の文書を iframe エディタで開ける
  - [ ] 編集権限のないユーザーは編集モードで開けない
  - [ ] 文書のバージョンを指定して開ける

### Task V2.3: 保存コールバック（WOPI類似）→ StorageService 保存
- **area**: docgen / **path**: `crates/api`, `crates/storage`
- **依存**: V2.2
- **仕様**:
  - エディタからの**保存コールバック（WOPI 類似）**を受ける API を実装し、編集結果を
    **StorageService 経由**で保存する（直バケット禁止・権限/監査/バージョニング適用）。
  - コンテンツアドレッシングで重複排除し、編集を新バージョンとして積む。
- **受け入れ条件**:
  - [ ] エディタでの保存が StorageService に新バージョンとして反映される
  - [ ] 保存が監査ログに記録される
  - [ ] バケット直アクセスを経由しない

### Task V2.4: 保存後の RAG 再索引トリガ
- **area**: docgen / **path**: `crates/storage`, `crates/rag`
- **依存**: V2.3
- **仕様**:
  - 保存による StorageService の**書込イベント**を契機に、編集後文書の RAG 増分再索引をトリガする
    （Phase 2 のインジェスト経路を再利用）。
  - 旧バージョンのチャンク失効と新バージョンの索引追加を整合させる。
- **受け入れ条件**:
  - [ ] ブラウザ内編集の保存後に検索が新内容を反映する
  - [ ] 旧バージョンのチャンクが検索に混入しない
  - [ ] 再索引が書込イベント経由で自動発火する

### Task V2.5: AI生成→手直し→保存の一連フロー結線
- **area**: docgen / **path**: `web/`, `crates/api`
- **依存**: V2.3
- **仕様**:
  - 資料作成 v1 の AI 生成物をエディタで開き、ユーザーが**手直し**して保存する
    一連の UX フローを結線する（生成 → 編集 → 保存 → 再索引）。
- **受け入れ条件**:
  - [ ] AI 生成した資料をそのままエディタで開いて編集できる
  - [ ] 手直し後の保存が v1 生成物と同じ node に新バージョンで載る
  - [ ] 生成→編集→保存がUIから連続して完結する

---

## トラックSAAS — マルチテナントSaaS 拡充（接頭辞 SAAS.x）

- **目的**: **SaaS は優先ターゲット**（requirements §1.1）。共有コントロールプレーン＋顧客ごと隔離 cell データプレーン（design §4.1.1）と
  認可コンテキストの `tenant_id` は **Phase 0 で day-1 導入済み**。本トラックはその上に
  **テナント分離の強制・オンボーディング自動化・課金/メータリング・プラン制限**を載せ、さらに
  将来の **データプレーン完全相乗り（フルプール）** へ寄せる拡張を扱う。
- **タイミング**: 課金/オンボーディング/プラン制限は SaaS 提供に向けて随時。フルプール化は需要が出たら。

### タスク一覧

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| SAAS.1 | テナント分離の強制（データ/authz/ストレージ境界） | infra | Phase 0（day-1 tenant_id） |
| SAAS.2 | テナント・オンボーディング（プロビジョニング自動化） | infra | SAAS.1 |
| SAAS.3 | 課金・使用量メータリング | infra | SAAS.1 |
| SAAS.4 | プラン制限・クォータ enforcement | infra | SAAS.3 |
| SAAS.5 | データプレーン完全相乗り（フルプール最適化・将来） | infra | SAAS.1 |

> ⚠️ 旧 SAAS.1「認可コンテキストへ `tenant_id` 追加」は **Phase 0 Task 0.5 に前倒し（day-1 で `AuthContext { principal, org, tenant_id }`）**。本トラックは tenant_id 継ぎ目の*実装*ではなく、その上の分離強制・運用・課金を扱う。

---

### Task SAAS.1: テナント分離の強制（データ/authz/ストレージ境界）
- **area**: infra / **path**: `crates/storage`, `crates/authz`, migrations
- **依存**: Phase 0（day-1 の `tenant_id` 付き認可コンテキスト）
- **決定（#84 / human）**: authz のテナント分離は **共有ストア＋識別子名前空間化**（フルプール）で実装する。
  FGA 識別子を `<type>:<tenant_id>|<local_id>` へ名前空間化し（区切り `|` = `authz::TENANT_SEP`）、
  生の識別子構築を `AuthContext::ns()`（[`authz::Namespace`]）のチョークポイントへ一本化して越境タプルを
  **型レベルで不能化**する。cell（顧客ごと専用ストア）は将来オプションとして残す（SAAS.5 と対）。
  → 旧 design.md「shiki データプレーン = 顧客ごと隔離セル」は authz については pool 採用へ更新（design.md §4.1）。
- **仕様**:
  - DB（行レベル/スキーマ）、authz store（識別子の tenant 名前空間化＝越境タプル不能化）、
    ストレージ（プレフィクス/バケット）、セッション（Redis キーの tenant_id スコープ）の各層でテナント境界を強制する。
  - cell 型では `tenant_id` 単一固定（`default`）でも動く後方互換（オンプレ＝シングルテナント）を保つ。
  - permission-aware RAG の検索/引用がテナント境界を越えないことを保証する。
  - 監査ハッシュチェーンも `tenant_id`＋org でスコープ（共用プール Postgres で越境連結しない）。
  - **オブジェクトストレージも tenant スコープ**: blob キー/PK を `{tenant_id}/{org}/{sha256}` へ（migration 0005・
    `content_address`）。同一 org slug を複数テナントが共有しても dedup 共有・hash 存在オラクル・refcount 破壊を防ぐ。
  - **`auth.tenancy=multi` の dev-only ゲート（`SHIKI_DEV_ALLOW_MULTI_TENANT`）を撤去**（全隔離層が tenant_id
    スコープになったため設定だけで運用可）。オンボーディング/課金/クォータ（SAAS.2〜4）は隔離とは独立の運用トラック。
- **受け入れ条件**:
  - [x] 全データアクセス・セッションが `tenant_id` 付きコンテキスト/キーを通る
  - [x] cell 型（単一テナント）・オンプレ構成が無変更で動作する（`tenant_id="default"` 名前空間で一様動作）
  - [x] あるテナントのデータが他テナントの検索/取得に一切現れない（authz タプルも境界を越えない）
  - [x] `tenant_id` 欠落／禁止文字／越境アクセスが fail-closed で拒否される
  - 注: SAAS.1 は #84（PR）で実装。role/部署共有（#76）はこの上に載る（PR-2）。

### Task SAAS.2: テナント・オンボーディング（プロビジョニング自動化）
- **area**: infra / **path**: `deploy/`, `crates/api`
- **依存**: SAAS.1
- **実装（#87 / #89）**: admin プレーン `POST/DELETE /admin/tenants`（provisioner service account の
  Bearer JWT ＋ azp 照合・設定なしなら fail-closed でルート不在）。tenant レジストリ
  （active→deleting→deleted・tombstone）・Keycloak admin REST（group/初期 admin・一時パスワード）・
  `StorageService::purge_tenant`（FGA タプル/オブジェクト/DB の整合撤去・audit は削除証跡として保持）。
  role メンバーシップはログイン時に IdP claims と **diff 同期**（reconciliation・離脱は次ログインで剥奪）。
  運用手順は `docs/guides/tenant-ops.md`。
- **受け入れ条件**:
  - [x] 新テナントを 1 操作で作成し初期 admin がログインできる
  - [x] テナント削除でデータ/authz/ストレージが整合的に撤去される
  - [x] プロビジョニングが冪等で再実行可能

### Task SAAS.3: 課金・使用量メータリング
- **area**: infra / **path**: `crates/api`, `crates/obs`
- **依存**: SAAS.1
- **仕様**:
  - テナント単位で使用量（トークン/ストレージ/ユーザー数/実行時間）を計測し、課金システムへ連携する。
  - llm-gateway のトークン会計（Phase 3）と監査を集計元に再利用する。
- **受け入れ条件**:
  - [ ] テナント別の使用量が正確に集計される
  - [ ] 集計が llm-gateway のトークン会計と一致する
  - [ ] 課金期間ごとの明細を出力できる

### Task SAAS.4: プラン制限・クォータ enforcement
- **area**: infra / **path**: `crates/api`, `crates/authz`
- **依存**: SAAS.3
- **仕様**:
  - プラン（Free/Pro/Enterprise 等）ごとの上限（ユーザー数・ストレージ・レート・機能フラグ）を定義し、
    認可コンテキスト評価時に enforcement する。
- **受け入れ条件**:
  - [ ] プラン上限を超える操作が拒否/抑制される
  - [ ] 機能フラグでプランごとに機能を出し分けられる
  - [ ] 上限到達がユーザーと管理者に通知される

### Task SAAS.5: データプレーン完全相乗り（フルプール最適化・将来）
- **area**: infra / **path**: `crates/storage`, `crates/authz`
- **依存**: SAAS.1
- **仕様**:
  - cell 隔離（顧客ごと隔離データプレーン）をやめ、**全テナント共有プール**へ寄せる。`tenant_id` 行分離を全層に全面適用しリソース効率を最大化する。
  - 強い隔離が要件の顧客向けには **cell（専用）を選べる二択**を残す（既定はプール）。
- **受け入れ条件**:
  - [x] 共有プールでテナント間データ漏れがゼロ（行/タプル/オブジェクト/セッション全層）— SAAS.1（#84）で達成
  - [x] cell（専用）とプール（相乗り）を構成で選択できる — `auth.tenancy=single/multi`
  - [x] 移行（cell→プール）がデータ整合を保って実行できる — `shiki-admin retenant`（#89・
    LEGACY→名前空間形式 / cell→pool の tenant リネーム。DB/FGA/オブジェクト/セッション一括・
    dry-run 既定・冪等。手順は `docs/guides/tenant-ops.md`）

---

## トラックGUI2 — 任意コード生成UI（iframe隔離）（接頭辞 GUI2.x）

- **目的**: Phase 6 の**宣言的コンポーネント・カタログが窮屈**と判明したとき、
  **任意の React/TSX をサンドボックス iframe 内で実行**できるようにする。
  厳格 CSP・親アクセス禁止・ネット遮断・postMessage ブローカーで隔離し、
  **全データアクセスは認可済みバックエンド経由**に限定する（アンビエント権限なし）。
- **タイミング**: **宣言的カタログ（Phase 6）が窮屈と判明したら**。安全な宣言的方式を先に尽くした後の手段。

### タスク一覧

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| GUI2.1 | サンドボックス iframe 実行環境＋厳格CSP | gui | – |
| GUI2.2 | 親アクセス禁止＋ネット遮断の隔離強制 | gui | GUI2.1 |
| GUI2.3 | postMessage ブローカー（認可済みバックエンド束縛） | gui | GUI2.2 |
| GUI2.4 | 任意 React/TSX ビルド＆ロードパイプライン | gui | GUI2.1 |
| GUI2.5 | 生成UIのアーティファクト化＆ReBAC共有 | gui | GUI2.3, GUI2.4 |

---

### Task GUI2.1: サンドボックス iframe 実行環境＋厳格CSP
- **area**: gui / **path**: `web/`
- **依存**: なし（Phase 6 の窮屈さが判明後に開始）
- **仕様**:
  - 生成 UI を **sandboxed iframe**（`sandbox` 属性最小化）で隔離実行する基盤を作る。
  - **厳格な CSP**（インラインスクリプト制限・許可 origin 限定・eval 方針）を適用する。
- **受け入れ条件**:
  - [ ] 生成 UI が sandboxed iframe 内でのみ描画される
  - [ ] CSP 違反のスクリプト/リソースがブロックされる
  - [ ] iframe が親と異なる origin/権限境界で動く

### Task GUI2.2: 親アクセス禁止＋ネット遮断の隔離強制
- **area**: gui / **path**: `web/`
- **依存**: GUI2.1
- **仕様**:
  - iframe 内コードから**親フレーム/親 DOM へのアクセスを禁止**し、
    **直接のネットワークアクセスを遮断**する（fetch/XHR/WebSocket を許可しない）。
  - 唯一の外部通路を後続の postMessage ブローカーに限定する。
- **受け入れ条件**:
  - [ ] iframe から親 window/DOM/Cookie にアクセスできない
  - [ ] iframe からの直接ネットワーク要求が遮断される
  - [ ] 隔離回避の試行がコンソール/監査で検出できる

### Task GUI2.3: postMessage ブローカー（認可済みバックエンド束縛）
- **area**: gui / **path**: `web/`, `crates/api`
- **依存**: GUI2.2
- **仕様**:
  - iframe ↔ ホスト間を **postMessage ブローカー**で仲介し、許可された**宣言済み・認可済み
    バックエンドアクションのみ**を呼べるようにする。**生成 UI にアンビエント権限を与えない**。
  - 全データアクセスは呼び出しユーザーの ReBAC 権限でバックエンド側が再評価する。
- **受け入れ条件**:
  - [ ] iframe は許可リストのバックエンドアクションのみ呼べる
  - [ ] データアクセスが呼び出しユーザーの権限で再評価される
  - [ ] 未宣言のアクション要求が拒否され監査に残る

### Task GUI2.4: 任意 React/TSX ビルド＆ロードパイプライン
- **area**: gui / **path**: `web/`, `crates/api`
- **依存**: GUI2.1
- **仕様**:
  - LLM 生成の **任意 React/TSX** を安全にビルド/バンドルし、iframe にロードするパイプラインを作る
    （依存固定・許可モジュールのみ・サイズ/時間上限）。
- **受け入れ条件**:
  - [ ] 生成 TSX が安全にビルドされ iframe で描画される
  - [ ] 許可外の依存/モジュールがビルド時に弾かれる
  - [ ] ビルド失敗時にユーザーへ安全にフォールバックする

### Task GUI2.5: 生成UIのアーティファクト化＆ReBAC共有
- **area**: gui / **path**: `web/`, `crates/api`, `crates/authz`
- **依存**: GUI2.3, GUI2.4
- **仕様**:
  - 任意コード生成 UI を**バージョン付きアーティファクト**化し（許可バックエンドアクション込み）、
    ミニアプリと同様に **ReBAC でロール共有**できるようにする。
- **受け入れ条件**:
  - [ ] 任意コード UI をアーティファクトとして保存/バージョン管理できる
  - [ ] ReBAC でロール/個人に共有・解除できる
  - [ ] 共有された UI も許可済みバックエンド経由でのみデータにアクセスする

---

## トラックBR — 会話ブランチUI（接頭辞 BR.x）

- **目的**: メッセージ編集/再生成による会話の**枝分かれ**と、ブランチ切替 UI を提供する。
  データ構造（`message.parent_id`）は **Phase 3 で用意済み**のため、主にバックエンドの分岐取得 API と
  フロントの可視化を実装する。
- **タイミング**: **任意**（需要に応じていつでも）。Phase 3 のスキーマがあれば独立着手できる。

### タスク一覧

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| BR.1 | ブランチ取得API（parent_id ツリー走査） | chat | 3.1 |
| BR.2 | メッセージ編集→新ブランチ生成 | chat | BR.1, 3.5 |
| BR.3 | 再生成→兄弟ブランチ生成 | chat | BR.1, 3.5 |
| BR.4 | ブランチ切替・ツリー可視化UI | frontend | BR.2, BR.3 |

---

### Task BR.1: ブランチ取得API（parent_id ツリー走査）
- **area**: chat / **path**: `crates/chat`, `crates/api`
- **依存**: 3.1（チャットドメイン: `message.parent_id`）
- **仕様**:
  - Phase 3 の `message(parent_id)` 構造を使い、スレッドの**分岐ツリー**を取得する API を実装する
    （現在の active path、各ノードの兄弟ブランチ一覧）。
- **受け入れ条件**:
  - [ ] スレッドのブランチツリーを API で取得できる
  - [ ] 任意ノードの兄弟ブランチを列挙できる
  - [ ] 既存の線形取得（active path）が引き続き動く

### Task BR.2: メッセージ編集→新ブランチ生成
- **area**: chat / **path**: `crates/chat`, `crates/api`
- **依存**: BR.1, 3.5
- **仕様**:
  - 過去のユーザーメッセージを**編集**すると、その親の下に**新しいブランチ**を生成し、
    編集後メッセージから応答を再実行する（元ブランチは保持）。
- **受け入れ条件**:
  - [ ] メッセージ編集が元を破壊せず新ブランチを作る
  - [ ] 新ブランチで agent-core が応答を生成する
  - [ ] 編集前ブランチに切り戻せる

### Task BR.3: 再生成→兄弟ブランチ生成
- **area**: chat / **path**: `crates/chat`, `crates/api`
- **依存**: BR.1, 3.5
- **仕様**:
  - アシスタント応答の**再生成**で、同じ親の下に**兄弟ブランチ**として別応答を追加する。
- **受け入れ条件**:
  - [ ] 再生成が既存応答を消さず兄弟ブランチを追加する
  - [ ] 複数回の再生成が並列ブランチとして残る
  - [ ] 各ブランチが個別に永続化される

### Task BR.4: ブランチ切替・ツリー可視化UI
- **area**: frontend / **path**: `web/`
- **依存**: BR.2, BR.3
- **仕様**:
  - 各分岐点で**ブランチ切替**（◁ 2/3 ▷ 等）を表示し、編集/再生成からの枝を辿れる UI を実装する。
  - 既定表示は線形（active path）、切替で兄弟ブランチに移動できる。
- **受け入れ条件**:
  - [ ] 分岐点でブランチを前後に切り替えられる
  - [ ] 編集/再生成で生まれた枝が UI から辿れる
  - [ ] 既定は線形表示でブランチが邪魔をしない

---

## トラックLR — LLM コストルーティング（接頭辞 LR.x）

> 詳細設計の正本は [docs/llm/model-router.md](../llm/model-router.md)（design §4.5.1 の詳細）。
> ⚠️ 着手前に [設計上の落とし穴](../design-caveats.md) の **PIT-28**（利用量＝金額クリティカル）・
> **PIT-45**（ルータの差し替えが会計から見えない）・**PIT-46**（自動降格の3方向の黙った破壊）を確認すること。

- **目的**: **コストカット**。「遂行すべきタスクを満たす最も安いモデル」を決定的なルールで選び、
  安価で通らなかったときだけ高いモデルに払う。学習型ルータに要る選好データが無い段階の現実解であり、
  同時に**将来の学習型ルータの教師データを運用そのものが生む**構造を作る（LR.6・LR.7）。
- **効果測定が先**: 既定は `shadow`（実行は従来どおり・決定と想定額だけ記録）。
  削減率・降格分布・昇格見込みを実データで確認してから `on` にする。品質退行リスクゼロで導入できる順序にしてある。
- **タイミング**: **3.2（llm-gateway）以降ならいつでも**。Phase の直線に乗らない独立トラック。
  LR.1〜LR.2 は前提工事（カタログのメタデータ化・複数プロバイダ）で、単体でも
  Task 12.1（モデルカタログ管理）の素地になるため先行着手の価値がある。
- **不変条件**: ルータは候補集合の中から選ぶだけで、**認可境界もレジデンシ境界も広げない**。
  明示選択（ユーザー / skill / ノード / アプリ）は上書きしない。ルータ自身は LLM を呼ばない。

### タスク一覧

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| LR.1 | モデルカタログのメタデータ化（tier/能力/context窓/residency/routable） | agent | 3.2 |
| LR.2 | 複数プロバイダ対応（`providers[]` ＋ `ModelEntry.provider`・単数設定の後方互換） | agent | 3.2 |
| LR.3 | `Router` トレイト＋`RuleRouter`（候補フィルタ・ルール表・決定的評価） | agent | LR.1, LR.2 |
| LR.4 | `RouteHint` 配線（agent-core / chat / workflow llm ノード / app-gateway） | agent | LR.3 |
| LR.5 | 実効モデルの会計貫通（`stream()` 戻り値・`llm_usage` 列追加・予算ガード） | agent | LR.3 |
| LR.6 | shadow モード＋削減レポート（baseline 差分集計・Langfuse metadata） | obs | LR.5 |
| LR.7 | 昇格（escalation）: 失敗シグナル→1段上げて再生成（上限・粘着） | agent | LR.4, LR.5 |
| LR.8 | 管理 UI: ルータ mode／ルール表／tier 編集（Task 12.1 に同居） | frontend | 12.1, LR.6 |

---

### Task LR.1: モデルカタログのメタデータ化
- **area**: agent / **path**: `crates/llm-gateway`, `crates/api`
- **依存**: 3.2
- **仕様**:
  - `ModelEntry` に `tier`（`small`/`standard`/`large`）・`context_window`・`max_output_tokens`・
    `capabilities { tools, vision, thinking, json_schema }`・`residency`（`domestic`/`overseas`）・
    `routable` を足す（[model-router.md](../llm/model-router.md) §3.1）。
  - `residency` はカタログの国外処理バッジ（design §4.12）と**同一語彙**を使う（表示とルーティング制約の二用途）。
  - **`tier` 未設定のモデルはルータ候補から外す**（fail-closed）。単価順からの自動導出はしない（PIT-46）。
- **受け入れ条件**:
  - [ ] 既存の設定ファイル（メタデータ無し）が無改修で読め、明示選択の挙動が変わらない
  - [ ] `tier` 未設定モデルが候補集合に現れない単体テストがある
  - [ ] `capabilities`/`context_window` を満たさない候補が機械的に落ちる単体テストがある

### Task LR.2: 複数プロバイダ対応
- **area**: agent / **path**: `crates/llm-gateway`, `crates/api`, `deploy/`
- **依存**: 3.2
- **仕様**:
  - `providers[]`（`name` 付き）へ拡張し、gateway は `HashMap<String, Arc<dyn LlmProvider>>` を保持する。
    `ModelEntry.provider` でモデルを provider に紐づける（省略時は既定 provider）。
  - **後方互換**: 単数 `provider` 設定は `name="default"` の 1 要素として読む。
  - 構成されていない provider に属するモデルは起動時のカタログ検証で候補から外す（**NFR-2 エアギャップ無傷**）。
- **受け入れ条件**:
  - [ ] 単数 provider 設定の既存構成・テストが無改修で通る
  - [ ] vLLM と外部 API を同時に構成し、モデル指定で送出先が切り替わる IT がある
  - [ ] 未構成 provider のモデルが候補にも既定にも選ばれない（起動時に検出され warn が出る）

### Task LR.3: `Router` トレイト＋`RuleRouter`
- **area**: agent / **path**: `crates/llm-gateway`
- **依存**: LR.1, LR.2
- **仕様**:
  - `Router` トレイト（`route(&RouteHint, &Catalog) -> RouteDecision`）と、その最初の実装 `RuleRouter`。
  - 決定手続きは [model-router.md](../llm/model-router.md) §5 の順序が正本
    （明示選択 → 候補集合 → ルール表 → 必要 tier → 同 tier 内で推定コスト最小 → 候補空なら既定へ warn 付き縮退）。
  - ルール表は**設定で編集可能**（宣言順・最初の一致が勝つ・`rule_id` を決定に載せる）。
  - **ルータは純関数**（LLM を呼ばない・状態を持たない・I/O をしない）。
- **受け入れ条件**:
  - [ ] 明示モデル指定時にルータが動かない単体テストがある
  - [ ] 同一入力に対し決定が完全に再現する（決定的である）単体テストがある
  - [ ] 必要 tier 以上のうち推定コスト最小が選ばれる単体テストがある
  - [ ] 候補が空のとき既定モデルへ縮退し理由がログに残る単体テストがある

### Task LR.4: `RouteHint` 配線
- **area**: agent / **path**: `crates/agent-core`, `crates/chat`, `crates/workflow-engine`, `crates/app-gateway`
- **依存**: LR.3
- **仕様**:
  - 4 呼出点が `TaskKind`・`step`・`tools_offered`・`requires_vision`・`effort`・`input_tokens_est` を宣言する
    （[model-router.md](../llm/model-router.md) §4）。トークン概算は agent-core の `estimate_tokens` を再利用する。
  - **同一 run 内の tier は単調非減少（粘着）**。降格判断は run 開始時と履歴剪定直後に限る（PIT-46・
    プレフィックスキャッシュ喪失の回避）。
- **受け入れ条件**:
  - [ ] 4 呼出点すべてが hint を渡す（hint 無しの経路が残っていない）
  - [ ] 同一 run 内で tier が下がらない IT がある
  - [ ] ツール提示ありの run が tools 非対応モデルへ降格しない negative IT がある

### Task LR.5: 実効モデルの会計貫通
- **area**: agent / **path**: `crates/llm-gateway`, `crates/agent-core`, `crates/chat`, `crates/api`, `migrations/`
- **依存**: LR.3
- **仕様**（**金額クリティカル・PIT-45**）:
  - `stream()` の戻り値を `RoutedStream { stream, decision }` にし、4 呼出点は `decision.model` を会計に使う。
  - `llm_usage` に `routed` / `route_rule` / `route_mode` / `requested_model` /
    `baseline_cost_usd_micros` / `escalation` を追加するマイグレーション。
  - 予算ガード（agent-core `Budget`）は実効モデル単価で積む。Langfuse metadata に決定を載せる。
- **受け入れ条件**:
  - [ ] 実効モデル≠要求モデルのとき `llm_usage.model` が実効側で記録される IT がある
  - [ ] `baseline_cost_usd_micros − cost_usd_micros` で削減額が集計できる IT がある
  - [ ] 予算ガードが実効モデル単価で発火する単体テストがある

### Task LR.6: shadow モード＋削減レポート
- **area**: obs / **path**: `crates/llm-gateway`, `crates/api`
- **依存**: LR.5
- **仕様**:
  - `router.mode = off | shadow | on`（**既定 `shadow`**）。テナント単位で上書き可。
  - `shadow` は**実行を一切変えず**、決定と想定額のみ記録する。
  - 集計 API/クエリ: 削減率・TaskKind 別分布・候補落ち率（カタログ設定の穴）・語彙ルール発火率・昇格兆候の発生率。
- **受け入れ条件**:
  - [ ] `shadow` で実行モデルが一切変わらない IT がある（PIT-45）
  - [ ] 削減率レポートが期間指定で取得できる
  - [ ] `off` で `llm_usage` のルータ列が付かず既存挙動と完全一致する

### Task LR.7: 昇格（escalation）
- **area**: agent / **path**: `crates/agent-core`, `crates/llm-gateway`
- **依存**: LR.4, LR.5
- **仕様**:
  - 失敗の兆候（ツール引数 JSON パース不能・空応答・`MaxTokens` 未完・ループ検出・ツールエラー N 回連続）で
    tier を 1 段上げて同ステップを再生成する（[model-router.md](../llm/model-router.md) §6）。
  - 上限: 1 run あたり K 回（初期値 2）・同一ステップ 1 回まで・昇格は単調。
  - **判断は agent-core（ステップ境界を持つ層）**が行い、ルータは段数を受け取って tier 下限を上げるだけ。
  - 再生成は別冪等キー（`:e{n}`）で刻む（計上漏れにしない・PIT-45）。
- **受け入れ条件**:
  - [ ] JSON 破損応答で 1 段昇格して再生成し、成功する IT がある
  - [ ] 昇格上限を超えない（無限に上がらない）IT がある
  - [ ] 昇格 1 回の run で生成回数分の `llm_usage` 行が残る IT がある

### Task LR.8: 管理 UI（ルータ設定）
- **area**: frontend / **path**: `web/`, `crates/api`
- **依存**: 12.1, LR.6
- **仕様**:
  - Task 12.1 のモデルカタログ管理画面に同居させる: `mode` の切替（off/shadow/on）・ルール表の編集・
    モデルごとの `tier`/`routable`/`residency` 編集・**削減額レポートの可視化**。
  - shadow の実測（削減率・降格分布）を同じ画面で見せ、**管理者が根拠を持って `on` に切り替えられる**導線にする。
- **受け入れ条件**:
  - [ ] 管理者が mode を切り替えられ、即座に次の呼び出しから反映される
  - [ ] ルール変更が保存でき、変更が監査に残る
  - [ ] shadow 期間の削減見込み額が期間指定で表示される
