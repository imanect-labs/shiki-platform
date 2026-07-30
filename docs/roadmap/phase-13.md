# Phase 13 — テーブル基盤（Teable / Microsoft Lists 級）

> 目的: Phase 9 の構造化データサービス（`crates/data`）を、ユーザーが直接作成・共有・編集できる
> 第一級の「テーブル」プロダクトへ昇格させ、チャット・generative UI・ワークフロー・shiki script・
> ミニアプリの全消費面から同一契約で使えるようにする。将来 PaaS（AI 製アプリのデプロイ基盤）の
> データ層を兼ねる。**設計正本: [table-platform.md](../table-platform.md)**（FR-18）。
>
> 依存: Phase 9（実装済み）・Phase 10 Stage B の data ノード（13.0）・Phase 6 genui・Phase 11-pre
> （CSV グリッドのフロント実装共有）。位置づけはアルファ後の最初の大型トラックだが、
> **13.0 のみアルファ内（Phase 10 の残タスク）**として先行する。
>
> ⚠️ 着手前に [design-caveats PIT-17〜21・PIT-53〜57](../design-caveats.md) を必ず確認。
> 完了の定義(DoD): 非エンジニアがテーブルを作成→フィールド定義→ビュー（grid/kanban/form）で共有→
> フォームで収集→承認 FSM＋ワークフロー通知、までを UI だけで組め、同じテーブルをチャットの AI が
> 読み書き（承認ゲート付き）し、genui 一覧が描画し、script/ワークフロー/ミニアプリが同一権限式で
> 操作できる。100 万行テーブルで索引フィルタ 1 ページ p95 < 100ms。

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 13.0 | 【アルファ内】workflow data ノード実行器＋ `Shiki.data.*` ブリッジ | wf/script | 9.x, 10 Stage B |
| 13.1 | フィールド安定キー（key/label 分離・論理削除・システムフィールド） | data | 9.2 |
| 13.2 | バッチ書込 API（atomic/非 atomic・冪等キー消費・ごみ箱） | data | 13.1 |
| 13.3 | keyset ページング・非索引フィルタガード・クォータ・性能包絡 | data | 13.1 |
| 13.4 | 型カタログ拡張（checkbox/url/email/rating/percent/currency/auto_number/attachment/multiple） | data | 13.1 |
| 13.5 | formula（式パーサ・型検査・read-time 評価器・除外強制） | data | 13.4 |
| 13.6 | rollup / backlink（権限透過集計・索引必須・走査上限） | data | 13.4 |
| 13.7 | スキーマ進化（安全変換行列・backfill ジョブ・索引状態機械） | data | 13.1 |
| 13.8 | テーブルホーム＋グリッド UI（無限スクロール・セル編集・409 自動リベース） | frontend | 13.2, 13.3, 11P |
| 13.9 | スキーマエディタ UI（フィールド/ポリシー/FSM 編集・AI 支援） | frontend | 13.7, 13.8 |
| 13.10 | ビュー拡張（kind 閉集合・kanban/gallery/calendar/form・submitter relation） | data/frontend | 13.8 |
| 13.11 | ライブ更新（outbox→SSE 粗通知・購読権限再チェック） | data/api | 13.8 |
| 13.12 | チャットツール群（data_* 読み書き・data_create_table・table_ref カード） | ai | 13.2, 13.3 |
| 13.13 | genui `table` / `record_form` コンポーネント＋宣言的アクション束縛 | gui | 13.10 |
| 13.14 | インポート/エクスポート（CSV/xlsx・jobq 冪等・エラーレポート・監査） | data | 13.2, 13.7 |
| 13.15 | ミニアプリ `table_refs`（既存テーブル参照の同意束縛） | app | 13.1, 9.13 |
| 13.16 | 監査計装・クォータ管理画面（export/import/schema_change/購読） | obs | 13.11, 13.14 |

---

## 詳細（受け入れ条件の要点のみ・仕様は設計正本を参照）

### Task 13.0: workflow data ノード実行器＋ Shiki.data ブリッジ【アルファ内】
- IR 語彙（`data.query`/`data.record.create`/`data.record.update`/`data.transition`）と script
  allowlist は定義済み。実行器/ブリッジを `crates/data` チョークポイント直結で実装する。
- [ ] ワークフローから record CRUD・遷移ができ、実行主体交差則（本人/委譲 ∩ スコープ ∩ ノード設定）が IT で担保される
- [ ] engine 冪等キーが data 側で消費され、ワーカー kill を挟むリトライで書込が高々 1 回（PIT-31）
- [ ] script から `Shiki.data.*` が同期スタイルで呼べ、認可・監査が通常経路と同一

### Task 13.1: フィールド安定キー
- [ ] 既存テーブルが `label = key` で無移行のまま読める（serde 後方互換）
- [ ] label rename がデータ・ビュー・ポリシー・formula を壊さない（key 参照の IT）
- [ ] フィールド論理削除で key が墓標予約され再利用が拒否される

### Task 13.2: バッチ書込 API
- [ ] `atomic:true` が単一 Tx で全成功/全失敗、`atomic:false` が op ごとの結果を返す
- [ ] 冪等キー再送が重複書込にならない
- [ ] 削除→ごみ箱→復元が行 authz・リビジョンと整合する

### Task 13.3: keyset ページング・ガード・クォータ
- [ ] 100 万行テーブルで索引フィルタ 1 ページ p95 < 100ms（ベンチを CI 外の計測ジョブに）
- [ ] 行数しきい値超テーブルの非索引 filter/sort が「インデックス必須」エラーになり、しきい値以下では動く
- [ ] §9 クォータ（行サイズ・ページ長・バッチ長）が fail-closed で強制される

### Task 13.4: 型カタログ拡張
- [ ] 新型の書込検証（url/email 形式・rating 範囲・attachment 可読検証）が効く
- [ ] auto_number が並行書込でも重複しない
- [ ] `multiple` な record_ref/file_ref の要素ごと参照整合が検証される

### Task 13.5: formula
- [ ] 型不一致・循環・深さ超過の式がスキーマ保存時に拒否される
- [ ] 評価エラーが null＋エラーマーカーで返る（黙って null にしない）
- [ ] formula フィールドが filter/sort/group_by/aggregate から除外される（PIT-19 機構の再利用・PIT-54）

### Task 13.6: rollup / backlink
- [ ] rollup が閲覧者の可読行のみを集計し、権限の異なる 2 ユーザーで値が変わる IT がある（PIT-57）
- [ ] source_ref_field の索引が無いと保存が拒否され、走査上限超過はエラーマーカーになる
- [ ] backlink 表示が参照元テーブルの行述語でフィルタされる

### Task 13.7: スキーマ進化
- [ ] 安全変換行列外の型変更が拒否され、行列内は backfill ジョブで完走・再開可能
- [ ] `CREATE INDEX CONCURRENTLY` の状態機械（pending/building/ready/failed）が動き、ready 前は索引前提クエリが解禁されない
- [ ] row_policy の緩和方向変更が監査に要注意イベントで残る

### Task 13.8〜13.10: UI（グリッド・スキーマエディタ・ビュー）
- [ ] CSV グリッドと実装共有した編集グリッドで、セル編集・rev 409 の自動リベース・範囲選択が動く
- [ ] form ビューが submitter relation のみのユーザーから送信でき、テーブル本体は読めない
- [ ] マスク対象フィールドがどのビュー設定でも応答から落ちる

### Task 13.11: ライブ更新
- [ ] SSE 通知に record id・値が含まれない（PIT-53 の存在オラクル遮断・golden テスト）
- [ ] 剥奪後 60 秒以内に購読が切断される
- [ ] 再フェッチ経路で行述語・マスクが常に再評価される

### Task 13.12: チャットツール
- [ ] 書込系・`data_create_table` が承認ゲードを通り、read 系が並列実行される（#349）
- [ ] AI 生成スキーマの不正（実在しない型・不正 formula）が検証で拒否されエラーがモデルへ返る
- [ ] `table_ref` カードからグリッド UI へ遷移できる

### Task 13.13: genui
- [ ] `table`/`record_form` が宣言的アクション経由でのみデータへ到達する（アンビエント権限なしの IT）
- [ ] 閲覧者の行述語・マスクがスペック作成者の権限と無関係に効く

### Task 13.14: インポート/エクスポート
- [ ] インポートジョブが途中失敗から再開でき、再実行しても行が重複しない（PIT-56）
- [ ] エクスポートがマスク・行述語適用済みで、`data.export` 監査が残る
- [ ] 行単位エラーレポートが成果物として保存される

### Task 13.15: ミニアプリ table_refs
- [ ] 同意画面が参照テーブルを明示し、同意なしでは到達できない
- [ ] 二重ゲート（束縛スコープ ∩ ユーザー ReBAC）が既存 capability_it と同型で担保される
- [ ] アンインストールで束縛が撤去されデータは残る

### Task 13.16: 監査・管理
- [ ] export/import/schema_change/購読イベントが trace_id で突合できる
- [ ] テナント管理者がクォータを参照・引き下げできる
