# Phase 6 — generative UI ＋ skill ＋ ミニアプリ

> ✅ **実装済み（2026-07-09・#187/#190/PR-3 の 3 本 stacked PR）**。以下の確定事項で 6.1〜6.12 を実装:
> - **新設 `crates/gui` が単一の信頼境界**: `UiSpecDoc` はタグ付き enum＋`deny_unknown_fields` でカタログ外
>   コンポーネント・生 HTML・未知 props を**構造的に表現不可**にし、その上の意味検証（深さ≤8・ノード≤200・
>   actions≤20・`https://` のみ・拒否のみ＝暗黙補正なし）を全経路（保存・emit・resolve）で共通適用（6.2/6.3）。
> - 生成は専用ツール **`emit_ui`**（検証失敗→`is_error`→モデル自己修正→テキストフォールバック。検証済みスペック
>   のみ `generative_ui` ブロックとして永続・SSE 配信）（6.4）。
> - アクションは**宣言的束縛の閉集合**（①安全ツール閉語彙 ②登録ハンドラ `chat.submit` ③workflow 対話トリガの
>   ピン版起動）。クライアントは `action_id+params` のみ送信・破壊系ツール束縛は保存 422＋実行時再チェックの
>   二重防御・全経路 `ui_action.invoke` 監査（6.5）。
> - skill = `SkillBody`（指示文＋知識スコープ＋許可ツール（縮小のみ）＋モデル既定＋few-shot＋script インライン）。
>   知識スコープは rag の pre-filter と**独立の AND 句**（TenantOnly 縮退でも維持・post-filter 個人 ReBAC 不変）。
>   適用は fail-closed（読めないピンは run 失敗）・**承認ポリシには不介入**（6.7/6.8/6.9）。
> - ミニアプリ = **常に明示ピン**の束（ui_spec/skill/workflows）＋**バンドル権限**
>   （`ArtifactStore::get_version_via_bundle`＝bundle viewer で部品を読む・部品の個別共有カスケード不採用）（6.10）。
> - web: 信頼カタログ→React の静的マッピング（`dangerouslySetInnerHTML`/eval 不使用・未知は縮退表示）・
>   チャート recharts・skill/アプリ管理 UI・ホームの skill ピッカー・Playwright e2e 3 本（6.6/6.11）。
> - 監査: `ui_spec.validate`(Deny)/`ui_action.invoke`/`skill.apply`/`miniapp.resolve`/`artifact.read_via_bundle`/
>   `rag.search` metadata scope（全て trace_id 付き）（6.12）。

> 📝 **2026-07-07 改訂**: 2点の設計更新。
> ①**workflow-engine が既にある前提で設計する**: Phase 10 Stage A（`crates/{durable,artifact,secrets,
> script-runtime,workflow-engine}`）が前倒し実装済みのため、本フェーズは「ワークフロー不在の暫定バックエンド束縛」
> ではなく、generative UI のアクションもミニアプリの実行主体も **workflow-engine を直接使う**前提で書く
> （Task 6.5/6.10 は Stage A の workflow-engine に直接依存し、Stage B を待たない。下記③との違いは末尾の※参照）。
> なお **Task 6.1（共有可能アーティファクト共通基盤）は Stage A の前提タスクとして既に `crates/artifact` に実装済み**
> （migration 0014/0024、`ArtifactKind`: workflow/ui_spec/mini_app/skill/script。`prompt_template` kind は
> #152 で撤去済み）。
> ②**旧 prompt template は skill に統合し呼称も定義も一本化する**（FR-7/FR-14 と統一）。skill artifact の中身は
> **SKILL.md 相当の指示文（用途・振る舞いを書く本文）＋知識スコープ／モデル既定／few-shot（旧 prompt template の
> 構成要素）＋（任意）script＋宣言ツール/スコープ＋（任意）参照資料**。script は **shiki script（`.shiki`。
> script-runtime で実行する ms 級グルーコード）と shell script（`.sh`。agent.invoke のサンドボックス内で実行する
> 重量級の自動化。Claude Code の skill と同じ `scripts/` 形式）のどちらも含められる**（1 skill に両方持たせてもよい）。
> skill 自体の呼び出し面は①チャット開始時の初期コンテキスト適用（本フェーズ・Task 6.7〜6.9）②エージェントへの
> ツールマウント③ワークフローの skill ノード、の3つ。**②③（skill store 経由の呼び出し）は Stage B・Task 10.11**。
> ※ Task 6.5/6.10 の workflow-engine 利用は「ミニアプリが束ねるワークフロー自体の起動」であり、
> 「skill を “他の” ワークフローの1ノードとして呼ぶ」（③）とは別物。前者のみ本フェーズで実装し、後者は Stage B。
>
> 目的: Phase 3 のチャット基盤の上に「共有可能アーティファクト」の系統を立ち上げる。LLM が出力した
> **検証済みJSONスペック**を信頼コンポーネント・カタログで描画する generative UI（任意コード実行なし）、
> RAG範囲を絞る skill、そして両者＋workflow-engine を束ねた **ミニアプリ**を、すべて
> 「アーティファクト＋バージョン管理＋ReBAC共有＋監査」の共通枠に乗せる。
> 完了の定義(DoD): 利用者が skill（SKILL.md相当の指示文＋知識スコープ＋許可ツール＋モデル既定）を
> 作ってロールに共有でき、チャット応答が generative_ui ブロックとしてフォーム/テーブル/チャート等を描画し、
> そのUIからの操作は**宣言済み・認可済みバックエンドアクション（サーバハンドラ束縛 or workflow-engine 対話トリガ起動）
> 経由のみ**で実行され（アンビエント権限なし）、skill＋UIスペック＋ワークフローをまとめた**ミニアプリ**がロールで
> 共有・実行できる。
> generative UI スペックは描画前に必ずスキーマ検証され、知識スコープで絞っても最終可読性は個人ReBACで再チェックされる。
>
> ⚠️ **テーブル（構造化データ・Phase 9）は本フェーズの対象外**。[miniapp-platform.md §6](../miniapp-platform.md)の
> 完全な定義（UIスペック＋テーブル＋ワークフロー＋skill＋script）に対し、本フェーズが届けるのはテーブルを除いた形。

## タスク一覧

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 6.1 | 共有可能アーティファクト共通基盤（version＋ReBAC＋監査）※Stage A前倒しで実装済み | gui | 3.7 |
| 6.2 | 信頼コンポーネント・カタログ定義＋UIスペックJSONスキーマ | gui | 6.1 |
| 6.3 | UIスペック検証層（描画前スキーマ検証・サニタイズ・拒否） | gui | 6.2 |
| 6.4 | generative_ui content block の実体化（生成→検証→保存） | gui | 6.3, 3.5 |
| 6.5 | 宣言的バックエンド束縛（認可済みアクション・workflow-engine対話トリガ起動含む・アンビエント権限なし） | gui | 6.4, 3.9 |
| 6.6 | generative UI レンダラ（カタログ描画・アクション送信） | frontend | 6.2, 6.5 |
| 6.7 | skill モデル＋バージョン管理（SKILL.md相当の指示文＋script） | gui | 6.1 |
| 6.8 | skill の知識スコープ適用（RAG範囲限定＋個人ReBAC再チェック） | gui | 6.7, 3.4 |
| 6.9 | skill の許可ツール／モデル既定／few-shot 実行統合 | gui | 6.7, 3.9 |
| 6.10 | ミニアプリ・アーティファクト（skill＋UIスペック＋ワークフロー。テーブルはPhase 9待ち） | gui | 6.5, 6.9 |
| 6.11 | skill／ミニアプリ管理UI（作成・共有・バージョン・実行） | frontend | 6.6, 6.7, 6.10 |
| 6.12 | generative UI／skill／ミニアプリの監査計装 | obs | 6.4, 6.8, 6.10, 3.8 |

---

## 詳細

### Task 6.1: 共有可能アーティファクト共通基盤 ※実装済み（Stage A前倒し）
> 📝 Phase 10 Stage A の前提タスクとして前倒し実装済み（`crates/artifact`・migration 0014）。以下は当初仕様の記録。
- **area**: gui / **path**: `crates/artifact`, `crates/authz`, migrations
- **依存**: 3.7
- **仕様**:
  - skill / UIスペック / ミニアプリ / ワークフロー / script を統一的に扱う `artifact(id, org, kind, owner, current_version, created_at)`
    ＋ `artifact_version(id, artifact_id, version, body JSONB, created_by, created_at)`。**不変バージョン**追記方式。
  - OpenFGA に `artifact` 型と relations（viewer/editor/owner）。個人/ロール共有・解除を Phase 3.7 と同じ ReBAC 枠で。
  - kind を持たせ Task 6.7/6.10 が同じテーブル・同じ共有API・同じ監査経路に乗る共通枠とする。
  - ✅ `ArtifactKind::PromptTemplate`（旧設計の名残）は skill への統合を受けて削除済み（`crates/artifact`・
    migration 0014）。`kind=skill` のみが正。
- **受け入れ条件**:
  - [ ] アーティファクトを作成し新バージョンを追記でき、過去バージョンが不変で取得できる
  - [ ] アーティファクトを個人/ロールに共有・解除でき、権限のないユーザーが参照できない
  - [ ] skill/UIスペック/ミニアプリが同一の共有・バージョン枠を共有する

### Task 6.2: 信頼コンポーネント・カタログ定義＋UIスペックJSONスキーマ
- **area**: gui / **path**: `crates/gui`
- **依存**: 6.1
- **仕様**:
  - **信頼されたコンポーネント・カタログ**を定義: form / table / chart / text-input / select / button / container / text 等。
    各コンポーネントの props を型定義し、**任意コード・任意HTML・任意イベントハンドラを許さない**宣言的スキーマに限定。
  - UIスペック = カタログ参照のツリー（component ＋ props ＋ 子）。アクションは Task 6.5 の宣言的バックエンド束縛 ID のみ参照可能。
  - スキーマは Rust 型を単一ソースに ts-rs/typeshare でフロントへ共有（手書き型なし）。
- **受け入れ条件**:
  - [ ] カタログにある各コンポーネントが型付き props で表現できる
  - [ ] カタログ外コンポーネント・生HTML・インラインコードはスキーマ上表現不可能
  - [ ] Rust 型から TS 型が生成され前後で一致する

### Task 6.3: UIスペック検証層
- **area**: gui / **path**: `crates/gui`
- **依存**: 6.2
- **仕様**:
  - LLM 出力 UIスペックを**描画前に必ず検証**: スキーマ適合・カタログ内コンポーネントのみ・props 型・参照アクションIDの存在・
    ネスト深さ/ノード数上限。違反は**拒否（描画しない）**しエラーを返す（部分描画・暗黙補正で危険物を通さない）。
  - 文字列値のサニタイズ（URL/リンクスキーム許可リスト等）。これは**信頼境界**であり保存・描画の双方の前段に置く。
- **受け入れ条件**:
  - [ ] スキーマ違反・カタログ外コンポーネントを含むスペックが拒否される
  - [ ] 存在しないアクションIDを参照するスペックが拒否される
  - [ ] 検証を通ったスペックのみが保存・描画に進む

### Task 6.4: generative_ui content block の実体化
- **area**: gui / **path**: `crates/gui`, `crates/agent-core`, `crates/chat`
- **依存**: 6.3, 3.5
- **仕様**:
  - Phase 3.6 でプレースホルダだった `generative_ui` content block を実体化。LLM/ツールが UIスペックを出力 →
    Task 6.3 で検証 → 検証済みスペックを content block として SSE 配信＆永続化（生スペックは保存しない／検証済みのみ）。
  - agent-core が UIスペック生成手段（専用ツール or 構造化出力）を持ち、generative_ui イベントを既存ストリームに流す。
- **受け入れ条件**:
  - [ ] チャット応答に検証済み generative_ui ブロックが含まれ SSE で配信される
  - [ ] 検証に失敗したスペックはブロック化されず安全に握りつぶされる（テキストでフォールバック）
  - [ ] 保存されるのは検証済みスペックのみ

### Task 6.5: 宣言的バックエンド束縛
- **area**: gui / **path**: `crates/gui`, `crates/api`, `crates/agent-core`, `crates/workflow-engine`
- **依存**: 6.4, 3.9
- **仕様**:
  - generative UI／ミニアプリのデータアクセス・操作は**宣言済みアクションの呼び出しのみ**。アクション = ①許可ツール
    ②明示登録のサーバ側ハンドラへの束縛　③**workflow-engine の対話トリガ起動**（ワークフローを持つミニアプリの場合。
    Task 6.10）のいずれか。**アンビエント権限を一切与えない**（UIが任意のAPI/DBを叩けない）。
  - アクション実行は常に**呼び出しユーザー自身の権限**で認可（ReBAC／Task 3.9 の破壊系・高コスト明示許可ポリシを継承。
    ③の場合は miniapp-platform §2.3 の対話トリガ実行主体モデル＝本人ReBAC ∩ ワークフロー宣言スコープ ∩ ノード設定）。
    UI側はアクションIDとパラメータのみ送れ、サーバが束縛定義に照合して認可・実行する。
- **受け入れ条件**:
  - [ ] UI からの操作は宣言済みアクション経由でしか実行できない
  - [ ] アクションは実行ユーザーの権限で認可され、無権限なら拒否される
  - [ ] 未宣言のエンドポイント/データへ UI から到達できない（アンビエント権限なし）
  - [ ] workflow-engine 対話トリガ起動のアクションも本人ReBAC∩ワークフロー宣言スコープ∩ノード設定で絞られる

### Task 6.6: generative UI レンダラ
- **area**: frontend / **path**: `web/`
- **依存**: 6.2, 6.5
- **仕様**:
  - 検証済み UIスペックを信頼コンポーネント・カタログの React 実装にマップして描画。`dangerouslySetInnerHTML`・
    eval・動的 import を使わない。フォーム送信/ボタン押下は Task 6.5 のアクションIDを叩く（直接 fetch 任意URL不可）。
  - 未知コンポーネント・未知 props は無視/プレースホルダ表示にフォールバック（クラッシュさせない）。
- **受け入れ条件**:
  - [ ] チャット内でフォーム/テーブル/チャートが描画される
  - [ ] フォーム送信が宣言済みアクションを呼び結果が反映される
  - [ ] スペックにカタログ外要素があってもクラッシュせず安全に縮退する

### Task 6.7: skill モデル＋バージョン管理
- **area**: gui / **path**: `crates/gui`, migrations
- **依存**: 6.1
- **仕様**:
  - skill body = **① SKILL.md 相当の指示文**（name/description のフロントマター＋用途・振る舞いを書く本文。
    Claude Code の skill と同型）② 知識スコープ（許可フォルダ/タグ） ③ 許可ツール ④ モデル/パラメータ既定
    ⑤（任意）few-shot ⑥（任意）script ⑦（任意）参照資料。**script は shiki script（`.shiki`）と shell script
    （`.sh`）のどちらも、また両方を同時に含められる**: `.shiki` は script-runtime（Task 10.8）で実行する
    ms 級グルーコード、`.sh` は agent.invoke のサンドボックス内で実行する重量級の自動化（Claude Code の
    `scripts/` と同じ位置づけ）。本タスクでは両形式ともファイル参照の保存のみ（実行は呼び出し面側）。
    Task 6.1 の artifact(kind=skill) として保存・**バージョン管理**。
  - ロール単位の共有は Task 6.1 の ReBAC を流用。skill 適用でチャット/ミニアプリの初期コンテキストを構成。
- **受け入れ条件**:
  - [ ] 上記要素を持つ skill を作成・更新（新バージョン）できる
  - [ ] skill をロールに共有でき、過去バージョンを参照できる
  - [ ] skill を選んでチャットを開始すると system/モデル既定が適用される

### Task 6.8: skill の知識スコープ適用
- **area**: gui / **path**: `crates/gui`, `crates/agent-core`, `crates/rag`
- **依存**: 6.7, 3.4
- **仕様**:
  - skill の知識スコープ（フォルダ/タグ）を doc_search の `scope` に反映し RAG 参照範囲を限定。
  - **限定はあくまで絞り込みで、最終可読性は常に呼び出しユーザー個人の ReBAC で再チェック**（Task 2.7/3.4）。
    スコープが広く設定されても他人の非可読文書は引用に出ない。
- **受け入れ条件**:
  - [ ] 知識スコープを設定すると doc_search の検索範囲がそのフォルダ/タグに絞られる
  - [ ] スコープ内でも個人に閲覧権限のない文書は引用に現れない
  - [ ] スコープ未設定時は従来通り全可読範囲を検索する

### Task 6.9: skill の許可ツール／モデル既定／few-shot 実行統合
- **area**: gui / **path**: `crates/gui`, `crates/agent-core`
- **依存**: 6.7, 3.9
- **仕様**:
  - skill の許可ツール集合を agent-core のツール提示に適用（Task 3.9 の全提示＋明示許可ポリシ配下で絞り込み）。
    モデル/パラメータ既定を llm-gateway 呼び出しに反映。few-shot をプロンプト先頭に注入。
  - 許可ツールに破壊系/高コスト系が含まれても Task 3.9 の明示許可が依然要求される（skill はそれを無効化しない）。
- **受け入れ条件**:
  - [ ] skill の許可ツールのみがそのセッションで使われる
  - [ ] モデル/パラメータ既定が適用され、few-shot が効く
  - [ ] skill 経由でも破壊系ツールは明示許可なしに実行されない

### Task 6.10: ミニアプリ・アーティファクト
- **area**: gui / **path**: `crates/gui`, `crates/api`, `crates/workflow-engine`
- **依存**: 6.5, 6.9
- **仕様**:
  - **ミニアプリ = skill ＋ UIスペック ＋ ワークフロー**のバージョン付きアーティファクト（artifact kind=mini_app）。
    Task 6.1 の version＋ReBAC＋監査の共通枠にそのまま乗せる。テーブル（構造化データ）は Phase 9 合流後に追加
    （完全な定義は [miniapp-platform.md §6](../miniapp-platform.md) が正本）。
  - ミニアプリ実行 = skill コンテキスト＋初期 UIスペック描画＋ワークフロー起動（対話トリガ・miniapp-platform §2.3の
    実行主体モデル）＋宣言済みアクションのみ実行。**アンビエント権限なし**。依存する skill/UIスペック/ワークフローの
    バージョンを固定参照（再現性）。
- **受け入れ条件**:
  - [ ] skill/UIスペック/ワークフローを束ねたミニアプリを作成・バージョン保存できる
  - [ ] ミニアプリをロールに共有し、共有相手が実行できる
  - [ ] ミニアプリからの操作が宣言済みアクション経由・実行者権限でのみ動く
  - [ ] ミニアプリのワークフロー起動が対話トリガの実行主体モデル（本人ReBAC∩宣言スコープ∩ノード設定）で絞られる

### Task 6.11: skill／ミニアプリ管理UI
- **area**: frontend / **path**: `web/`
- **依存**: 6.6, 6.7, 6.10
- **仕様**:
  - skill の作成/編集（SKILL.md本文＋知識スコープ＋許可ツール＋モデル既定＋few-shot＋任意script のフォーム）、
    バージョン履歴、共有ダイアログ。
  - ミニアプリの一覧/作成/実行（Task 6.6 レンダラ埋め込み）、共有、バージョン切替。チャットから skill/ミニアプリ選択。
- **受け入れ条件**:
  - [ ] UIから skill を作成・共有・バージョン切替できる
  - [ ] UIからミニアプリを作成・共有・実行できる
  - [ ] チャット開始時に skill/ミニアプリを選択できる

### Task 6.12: generative UI／skill／ミニアプリの監査計装
- **area**: obs / **path**: `crates/gui`, `crates/agent-core`, `crates/api`
- **依存**: 6.4, 6.8, 6.10, 3.8
- **仕様**:
  - 共通枠の操作を監査記録: アーティファクト作成/更新/共有、適用した skill バージョン、生成 UIスペックの検証結果、
    実行された宣言的アクション（誰が・どの権限で・どの束縛を、ワークフロー起動を含む）、引用chunk。
    Task 3.8 の trace_id で Langfuse/OTel と相関。
  - UIスペック検証の拒否・アクション認可拒否も監査に残す（セキュリティ事象の追跡可能性）。
- **受け入れ条件**:
  - [ ] skill 適用・ミニアプリ実行・UIアクションが監査ログに残り trace_id で辿れる
  - [ ] UIスペック検証の拒否／アクション認可拒否が記録される
  - [ ] 「誰が・どの権限で・どのアクション/引用を得たか」が同一 trace_id で突合できる
