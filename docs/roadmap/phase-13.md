# Phase 13 — HTML ページ／サイト公開（アーティファクト・CMS・フォーム・マーケティング）

> 目的: **AI が資料を HTML で組む「アーティファクト」**と、**対外的な Web サイトの作成・公開**を、
> 一つの基盤（`.page` ＝ Yjs collab ドキュメント）の上に載せる（design §4.13）。
> 保存実体・エディタ・レンダラ・砂箱・AI 編集ツールは共有し、束ねる器（`site`）だけを分ける。
> 公開は **FGA を通さない匿名配信**（app-gateway 第4リスナ）と **既存の Session＋FGA 経路**の 2 経路に分け、
> 前者は B1（design §4.10）と同型に `AuthContext` を作らない。**インデックスは既定 noindex**。
> スライド（Phase 11）とは独立の機能だが、基盤は §4.8.3 からそのまま流用する。
>
> 完了の定義(DoD): チャット「この内容を HTML の資料にして」→ページ下書き→ドライブ保存→3タブエディタで編集→
> サイトへ配置→publish→`{site}.sites.<domain>` で匿名閲覧（既定 noindex）→問い合わせフォーム送信が
> `data_table` に入りワークフローが起動→ロールバックで前版に戻る、が一続きで動く。
>
> **スコープ外**: スライド/ノート単体の Web 公開（publication のマニフェストは汎用に保つが静的化器は作らない）・
> 予約公開・カスタム 404/リダイレクト・匿名向けパスワード保護ページ・公開前プレビューの外部共有・
> `data_table` からの構造化コンテンツ生成。いずれも publication とバンドルの形を変えずに後から足せる。
>
> ⚠️ **着手前に [設計上の落とし穴](../design-caveats.md) の PIT-58（匿名の書き込み面）・PIT-59（オリジン隔離）・
> PIT-60（公開スナップショットの参照断ち・キャッシュ）・PIT-61（noindex の既定）・
> PIT-62（`form-action`／`base-uri`／`worker-src`）を確認すること。**
> 併せて PIT-45（org＝テナント内の隔離境界）・PIT-46（匿名/テナント跨ぎ共有の安全包絡）・
> PIT-40（生 HTML をアプリオリジンでレンダリングしない）・PIT-41（Y.Text 文字マージによる HTML 構文破壊）も前提になる。
>
> 🔒 **human 承認が要るタスク**:
> - **13.8 の「匿名公開の有効化」** — design §4.13 は匿名公開の解禁自体を PIT-46 と同じ包絡に置き、
>   現段階は同一テナント内限定としている。**リスナの実装は既定 OFF で進めてよいが、匿名到達を有効化する構成変更は承認事項**。
>   13.8 が 13.12 より先に来るため、これを分けないと Stage 2 と DoD を進めるだけで未承認の匿名閲覧が解禁されてしまう。
> - **13.12**（匿名 `PrincipalKind` の新設と匿名面での DB 書き込み）— 同じく PIT-46 の包絡。
> - **13.6**（ワイルドカード DNS／証明書／ingress）— リポジトリ外インフラの新規整備であり、Phase 8 Task 8.8 と調整が要る。
>
> DoD の「匿名閲覧」も上記の承認を前提とする（承認前は既定 OFF のまま、限定共有経路で同じ内容を確認する）。

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| **Stage 1 — アーティファクト（インフラ変更ゼロ）** | | | |
| 13.1 | ページ doc 種（`DocKind::Page`・`.page`・serialize/saver・RAG）＋閲覧ビュー | storage/frontend | 11.1 |
| 13.2 | 3タブエディタ（プレビュー/ビジュアル/コード・CodeMirror 6・GrapesJS 砂箱の一般化） | frontend | 13.1, 11.2 |
| 13.3 | AI ページ生成/編集（`page.read`/`page.edit`・`save_page` 下書き確定型） | ai | 13.2, 11.3 |
| 13.4 | テーマモデル（CSS 変数とプロンプトへの単一注入点・アーティファクト規範） | frontend/ai | 13.3 |
| 13.5 | ドライブ導線（新規作成・拡張子ルーティング・共有リンク閲覧） | frontend | 13.1 |
| **Stage 2 — 公開基盤（初めてインフラに手を入れる）** | | | |
| 13.6 | インフラ: ワイルドカード DNS／証明書／ingress＋機能フラグ 🔒 | infra | 8.8 |
| 13.7 | `crates/site`: site/publication モデル＋publish パイプライン（参照断ち） | storage | 13.1 |
| 13.8 | sites 配信リスナ（第4リスナ・`AuthContext` なし・ホスト名解決・sha 照合）＋既定 OFF 🔒 | api | 13.7, 13.6 |
| 13.9 | `site_csp()`＋`X-Robots-Tag`＋`robots.txt`＋`sitemap.xml`＋OGP/favicon | api | 13.8 |
| 13.10 | 不透明 ID 発番＋vanity 申請＋インデックス許可の二重ゲート | api/frontend | 13.8 |
| 13.11 | `/sites` 管理面（公開状態・ロールバック・プレビュー URL） | frontend | 13.8 |
| **Stage 3 — フォーム＋CMS＋アニメ CDN** | | | |
| 13.12 | 匿名 `PrincipalKind`＋フォーム受付（レート制限・冪等台帳・監査） 🔒 | auth/api | 13.8 |
| 13.13 | `form` メタ＋生 HTML マッピング検証＋`data-shiki-form` 埋込タグ | api/frontend | 13.12, 9.x |
| 13.14 | `data.record.created` の outbox 発行＋`EventSource` 解禁（フォーム→ワークフロー） | api | 13.12, 10.x |
| 13.15 | ノートコレクション＋テンプレート適用＋ビルド時生成（一覧/静的検索） | site | 13.7, 11P.x |
| 13.16 | シキ製アニメ CSS ライブラリ＋バージョン付き自前 CDN 配信 | frontend | 13.8 |
| **Stage 4 — マーケティング** | | | |
| 13.17 | A/B テスト（cookie なし割当・キャッシュ戦略） | api | 13.8 |
| 13.18 | `site_event` 計測＋ビーコン＋jobq 集計＋ダッシュボード（既定 OFF） | api/frontend | **13.12**, 13.8 |
| 13.19 | 送信データの CSV エクスポート | data | 13.13 |
| 13.20 | 独自ドメイン（ACME on-demand＋CNAME 所有確認） | infra | 13.6, 13.10 |

---

## 詳細

### Task 13.1: ページ doc 種＋閲覧ビュー
- **area**: storage/frontend / **path**: `crates/collab`, `crates/api`, `web/`, `ingestion-worker/`
- **仕様**: `DocKind` 閉集合に `Page` を追加（`.page`・MIME `application/vnd.shiki.page+json`）。
  真実は Yjs（`Map "meta"` はノート/スライドと同一マップ名/型を共用、本文は `Y.Text` の HTML 1 本）。
  保存時に正規化 JSON へシリアライズ→`update_file_content_internal`（版/監査/outbox/RAG の既存経路）。
  **スライドと違いサニタイズは掛けない**（任意 JS を許すため。守りはオリジン分離と CSP・PIT-59）が、
  サイズ上限は掛ける。`parse.py` に `.page` ハンドラ（既存 html パスへ）。
  閲覧は `/pages/{id}` ＋ **別オリジン iframe**（アプリオリジンで生 HTML をレンダリングしない・PIT-40）。
  **PIT-41 と同型の HTML 整合性防御を入れる**: 本文が単一 `Y.Text` である以上、文字粒度マージでタグが割れるので、
  取り込み時に DOMParser で parse→serialize 正規化して自己修復させる（収束するだけでは構文健全性は保証されない）。
- **受け入れ条件**:
  - [ ] `.page` の Yjs 編集が保存で新バージョンになり、RAG 検索に本文が乗る
  - [ ] serialize 往復（JSON⇄Yjs）が壊れない
  - [ ] `<script>` 入りの `.page` を直接アップロードしても、アプリオリジンでは一切実行されない（e2e negative）
  - [ ] タグ境界をまたぐ並行編集を与えても、収束後の本文が**常にパース可能**（PIT-41 同型の adversarial テスト）

### Task 13.2: 3タブエディタ
- **area**: frontend / **path**: `web/`, `web/editor-sandbox/`, `crates/app-gateway`
- **仕様**: 1 ページ＝1 HTML ドキュメントで、タブは**プレビュー／ビジュアル／コード**。
  ビジュアルは既存 GrapesJS 砂箱（`/builtin/slide-editor`）をページ向けに一般化して別バンドルで配信
  （閉集合の許可名に追加・`builtin_csp` の通信全遮断は維持）。コードは **CodeMirror 6** を新規導入し
  `y-codemirror.next` で Yjs へ直結（TipTap＋y-prosemirror・GrapesJS＋`slides-doc.ts` と同型）。
  プレビューは opaque origin iframe を既定とし、実機同等の確認用に sites オリジンの短命プレビュー URL を用意する。
  共通シェル（`usePageHeader`・没入モード・`EditorLoading`）と選択→AI（`SelectionContext`）は既存を再利用。
- **受け入れ条件**:
  - [ ] 3タブ間の往復で HTML が壊れない（ビジュアル編集→コード→ビジュアル）
  - [ ] 2 ユーザーの同時編集が収束し、**収束後の本文が常にパース可能**（e2e 2 コンテキスト・PIT-41 同型）
  - [ ] CodeMirror・GrapesJS・AI が同じタグ周辺を同時に触っても構文が壊れない（編集中プレゼンスで衝突を可視化）
  - [ ] viewer 権限では編集 UI が無効で、書込が届かない
  - [ ] プレビューがアプリオリジンではない（CSP/オリジンの golden テスト）

### Task 13.3: AI ページ生成/編集
- **area**: ai / **path**: `crates/chat`, `crates/agent-core`, `crates/collab`, `web/`
- **仕様**: `save_page` を**下書き確定型**（`save_slide` と同型: 下書きカード→`/pages/draft`→「ドライブに保存」）。
  `ToolOutcome` / `ContentBlock` / `StreamEventKind` の 3 箇所へ draft 種を同期追加し、
  フロントは `createDraftStore("page")` を足す。編集は `page.edit` が編集 op を Yjs トランザクションとして発行
  （editor relation・HigherConsistency・人間と同一経路・排他なし）。`page.read` も併せて公開。
- **受け入れ条件**:
  - [ ] 「この内容を HTML の資料にして」→下書き画面→保存→`/pages/{id}` が一続きで動く（e2e）
  - [ ] 人間の編集中に AI が編集しても収束し、AI 名義で表示される
  - [ ] editor 権限のない実行主体の `page.edit` が拒否される

### Task 13.4: テーマモデルとデザイン規範
- **area**: frontend/ai / **path**: `crates/site`, `crates/chat`, `web/`
- **仕様**: テーマ（色・フォント・角丸・余白スケール・アニメの強さ）を単一の値集合として定義し、
  **CSS カスタムプロパティとして生成 HTML へ焼く経路**と、**プロンプトへ注入する経路**の両方を 1 箇所から導出する
  （`slide_templates.rs` の `THEMES` / `design_guidance()` と同型）。アーティファクトは `globals.css` の
  シキのデザイン言語（Deep Navy・四季アクセント・`shiki-dash`・`palt`）を既定テーマとし、
  サイトは `site` メタのテーマを使う。
- **受け入れ条件**:
  - [ ] テーマ値の変更が CSS とプロンプトの両方へ同時に反映される（単一定義のテスト）
  - [ ] アーティファクトが既定でシキのトークンに沿った見た目になる（視覚確認・2x/両テーマ）

### Task 13.5: ドライブ導線
- **area**: frontend / **path**: `web/`
- **仕様**: `use-create-content.ts` に `.page` の新規作成を追加し、ドライブの拡張子→エディタ分岐と
  「新規作成」メニュー、チャットの「＋ → 作成」に載せる。既存の共有リンク（発行/失効/パスワード/公開範囲）で
  閲覧できるようにする（公開機能はまだ無い）。
- **受け入れ条件**:
  - [ ] ドライブから `.page` を作成・開く・共有リンクで閲覧、が動く（e2e）
  - [ ] 共有リンク経由の閲覧でも生 HTML がアプリオリジンで実行されない

### Task 13.6: 公開インフラ 🔒
- **area**: infra / **path**: `deploy/`
- **仕様**: `*.sites.<domain>` のワイルドカード DNS とワイルドカード証明書、ingress（TLS 終端＋Host ルーティング）を
  整備する。リポジトリには現状リバースプロキシ/ingress/TLS の構成が存在しないため、Phase 8 Task 8.8（IaC）と
  調整して置き場所を決める。**構成が揃わない環境ではサイト公開を機能フラグで無効化**し、
  パスベースへ縮退させない（PIT-59）。開発環境のパスベースは dev 専用として、本番構成では選べない形にする。
  **併せてクライアント IP の信頼境界を定義する**: sites リスナは ingress の背後に置かれるため、
  ソケットの peer IP では全訪問者が 1 アドレスに潰れ、`X-Forwarded-For` を無条件に信じれば偽装で回避される。
  ingress がヘッダを上書きし、設定済みの proxy hop からのみ実クライアント IP を採る契約を構成として持つ
  （リポジトリに trusted-proxy 抽出の前例が無いので本タスクが初出。13.12/13.18 のレート制限がこれに依存する）。
- **受け入れ条件**:
  - [ ] `{site}.sites.<domain>` が TLS で解決し、sites リスナへ到達する
  - [ ] ワイルドカード未構成の構成でサイト公開 API が機能無効として拒否される
  - [ ] dev のパスベース設定が本番プロファイルで選択できない
  - [ ] 信頼済み hop 以外から来た `X-Forwarded-For` が採用されない（spoofing negative テスト）

### Task 13.7: `crates/site` とパブリッシュパイプライン
- **area**: storage / **path**: `crates/site`, `crates/storage`, `migrations/`
- **仕様**: `site`（フォルダ node を指すメタ: slug・ドメイン・テーマ・公開設定）と `publication`
  （不変スナップショット: マニフェスト `path → {sha256, content_type}`・`indexable`・active フラグ）を新設。
  **migration は 0061 以降**（過去に番号衝突で `VersionMismatch` によりサーバ起動不能になった事故が
  `0060_node_system.sql` のコメントにある。採番前に必ず `ls migrations` で確認）。
  publish はページ HTML と参照アセットを content-address でバンドルへ**コピーして参照を断つ**（PIT-60）。
  キーは `crates/storage/src/content_address.rs` に `site_bundle_key()` として単一定義。
  読めないアセットや別 org のファイル参照は publish を失敗させる。ロールバックは active ポインタの差し替え。
- **受け入れ条件**:
  - [ ] publish 後にドライブ上の元アセットを削除・差し替えても公開ページが変わらない
  - [ ] 読めないアセット／別 org のファイルを含むページの publish が理由付きで失敗する
  - [ ] ロールバックで直前の publication が active になる
  - [ ] unpublish で配信が止まる

### Task 13.8: sites 配信リスナ（第4リスナ）🔒
- **area**: api / **path**: `crates/app-gateway`, `crates/site`, `crates/api/src/wiring_gateway.rs`
- **仕様**: `b1.rs` と同型に**認証抽出を一切持たない**リスナを新設する。処理は
  「Host → site 解決 → active publication → マニフェスト引き → blob 取得 → sha256 再計算照合 → 返却」だけ。
  **リスナは DB と ObjectStore を直接触らず、`crates/site` の `PublicSiteService` だけを呼ぶ**
  （FGA を引かないことと、チョークポイントを持たないことは別・PIT-59）。`X-Content-Type-Options: nosniff`。
  **キャッシュは URL の性質で二分する**（PIT-60）: HTML と active マニフェストは `no-cache` ＋ publication の ETag、
  `immutable` は URL に sha を含めたアセットのみ。B1 のヘッダをそのまま写経すると unpublish が CDN に届かない。
  存在しない/未公開のサイトは存在を秘匿して 404。**ホスト名でのルーティングはリポジトリで初出**なので、
  Host パースと site 解決を単一関数に閉じ、cookie は読まない（セッションはテナントを cookie 値に埋め込む方式のまま）。
  🔒 **匿名到達は既定 OFF**で実装する。有効化する構成変更は human 承認事項（PIT-46 の包絡）。
- **受け入れ条件**:
  - [ ] リスナのコードに認証抽出・cookie 参照が存在しない（構造テスト）
  - [ ] リスナが sqlx / ObjectStore を直接呼ばず `PublicSiteService` 経由である（構造テスト）
  - [ ] 2 つのサイトが別オリジンで配信され、片方の JS がもう片方の storage に到達できない（e2e negative）
  - [ ] マニフェストと実体の sha が食い違うと配信されない
  - [ ] 未公開・存在しないサイトが 404 で、存在の有無を区別できない
  - [ ] HTML に `immutable` が付かず、ロールバック直後の再取得で新しい publication が返る
  - [ ] 既定構成では匿名到達が無効（有効化には明示的な構成が要る）

### Task 13.9: CSP・robots・sitemap・OGP
- **area**: api / **path**: `crates/app-gateway`, `crates/site`
- **仕様**: `site_csp()` を `bundle_csp()`/`builtin_csp()` と同型の純粋関数として実装し golden テストで固定
  （self ＋ シキ CDN のみ、`connect-src` はフォーム受付と計測ビーコンの 2 宛先、外部 CDN と外部 fetch は禁止）。
  **`default-src` のフォールバック対象外の 3 つを明示する**（PIT-62）: `form-action` はシキの受付先のみ
  （`connect-src` は `<form>` 送信を止めない）、`base-uri` は自オリジン固定、`worker-src 'none'`
  （Service Worker は publication 切替後も生き残り、ロールバックと unpublish を無効化する）。
  `X-Robots-Tag: noindex, nofollow` を既定付与、`robots.txt` は既定 `Disallow: /`、
  `sitemap.xml` は indexable なページからのみ生成。OGP/favicon は `site` とページのメタから生成する。
- **受け入れ条件**:
  - [ ] `site_csp()` の golden テストがあり、`form-action`・`base-uri`・`worker-src` を含む
  - [ ] 外部ホストへの fetch と外部 CDN の読み込みが CSP で落ちる
  - [ ] 外部 `action` を持つ `<form>` の送信が落ち、`<base>` による外部への付け替えも効かない（negative）
  - [ ] publication 内のスクリプトが Service Worker を登録できない（negative）
  - [ ] 既定で publish したページに `X-Robots-Tag: noindex` が付き、`robots.txt` が全 Disallow
  - [ ] sitemap に noindex のページが載らない

### Task 13.10: 名前の発番とインデックス許可
- **area**: api/frontend / **path**: `crates/site`, `crates/api`, `web/`
- **仕様**: publish 時に**不透明 ID を発番**して既定のサブドメインにする。vanity 名はグローバル先着＋予約語リストで
  申請制（テナント名はホストに出さない）。`indexable = true` にできるのは「公開範囲＝匿名」かつ
  「テナント管理者がテナント設定で許可（既定 OFF）」の**二重ゲート**が揃ったときだけで、
  既定値は publication に**焼き込む**（配信時に現在の設定を参照しない・PIT-61）。
- **受け入れ条件**:
  - [ ] テナント設定が OFF のまま `indexable=true` にできない
  - [ ] 公開範囲が匿名でない publication は `indexable=true` にできない
  - [ ] 既定値を後から変えても過去の publication の `indexable` が変わらない
  - [ ] 予約語と既存 vanity の重複が拒否される
  - [ ] **許可側の e2e**: 二重ゲートを満たした publication では noindex ヘッダが外れ、`robots.txt` が許可し、
        sitemap に載る（否定条件だけだと「常に noindex を返す実装」が全条件を通ってしまう・PIT-61）

### Task 13.11: `/sites` 管理面
- **area**: frontend / **path**: `web/`
- **仕様**: 公開中サイトの一覧（公開状態・URL・最終公開日時・インデックス可否）、publish/unpublish、
  ロールバック、短命プレビュー URL の発行を 1 画面に集約する。ページ自体はドライブに置いたままで、
  ページ用の専用ギャラリーは作らない（二重の置き場を作らないため）。
- **受け入れ条件**:
  - [ ] 一覧から publish/unpublish/ロールバックができる（e2e）
  - [ ] 公開状態とインデックス可否が一覧で判別できる

### Task 13.12: 匿名 principal とフォーム受付 🔒
- **area**: auth/api / **path**: `crates/api/src/extract`, `crates/app-gateway`, `crates/site`, `migrations/`
- **仕様**: `PrincipalKind` に匿名種別を追加し（`PrincipalKind::Workflow` を足した migration 0022 と同型）、
  **tenant/org/table は `form` 定義からのみ解決**する（リクエスト由来の値は 1 つも採用しない）。
  受付ルートは**メイン API の `route_table()` に足さず sites リスナ側**に置く。
  監査は form に紐づく合成 actor で必ず残す。ハニーポットと最小滞在時間で自動投稿を落とす。
  `share_link_redeem.rs` から流用するのは**構造**（ロック外での重い検証・advisory lock による直列化・deny 台帳・
  理由を区別しない失敗）であって limiter の実装ではない — `share_link_ratelimit.rs` は自ら
  「プロセス内 best-effort（各レプリカ独立）」と宣言しており、匿名面に流用すると許容量がレプリカ数だけ増える。
  **レート制限は workflow-engine の共有トークンバケット**（design §4.12 で API 面へ適用済み）を
  IP と form_id の双方に使い、**実クライアント IP は 13.6 の trusted-proxy 境界から採る**。
  **冪等性は 2 段構え**（PIT-58）: JS がある経路は受付が発行する短命の署名済み送信トークン、
  no-JS 経路は `(form_id, 正規化ペイロードのハッシュ, クライアント IP ハッシュ)` を鍵に分単位の窓で重複排除する
  （静的 publication に焼かれた HTML は閲覧ごとに一意な鍵を埋め込めないため）。**PIT-58 を必読**。
- **受け入れ条件**:
  - [ ] ボディやヘッダで別テナント/別テーブルを指定しても form 定義の宛先にしか入らない
  - [ ] 同一冪等キーの二度目が行を増やさない
  - [ ] JS 無効の同一内容再送が窓の中で 1 件に潰れ、窓を越えれば通る
  - [ ] レート制限超過が理由を区別せず 429
  - [ ] レプリカを増やしても合計の許容量が増えない（分散 limiter の結合テスト）
  - [ ] 信頼済み hop 以外から来た `X-Forwarded-For` でレート制限を回避できない
  - [ ] 送信ごとに監査が 1 件残る
  - [ ] メイン API の `route_table()` に新しい `Public` ルートが増えていない（構造テスト）

### Task 13.13: フォーム定義と埋め込み
- **area**: api/frontend / **path**: `crates/site`, `crates/gui`, `web/`
- **仕様**: `form` メタテーブル（`data_table` を指す・公開フィールドのサブセット・既定値）を新設。
  正本は**生 HTML の `<form>`**（`action` が受付エンドポイント・JS 無効でも動く）で、送信フィールド名は
  自己申告として扱い `form` 定義に照らして検証する（`crates/data` のスキーマ検証を再利用・未宣言は拒否）。
  ノーコード用に `<div data-shiki-form="...">` の埋込タグを併設し、自前 CDN の JS が genui の Form を描画する。
- **受け入れ条件**:
  - [ ] 生 HTML の `<form>` から送信した行が `data_table` に入る（e2e）
  - [ ] `form` が公開宣言していないフィールドが拒否される
  - [ ] 埋込タグが同じ `form` 定義から描画され、同じ検証を通る
  - [ ] JS を無効にしても生 HTML のフォームが送信でき、**その経路の冪等性が 13.12 の縮退仕様どおり**
        （窓内の同一内容再送は 1 件・窓外は通る）に振る舞う

### Task 13.14: フォーム→ワークフロー
- **area**: api / **path**: `crates/data`, `crates/workflow-engine`, `crates/api`
- **仕様**: `crates/data` のレコード作成で `data.record.created` を**同一トランザクションで outbox へ発行**し、
  `EventSource` の `available_stage_a()` を拡張して当該ソースを解禁する。フォーム送信を契機に通知・自動返信・
  集計のワークフローが回る。at-least-once（PIT-31）前提なので、副作用側の冪等は既存規約に従う。
- **受け入れ条件**:
  - [ ] フォーム送信でワークフローが起動する（結合テスト）
  - [ ] outbox 発行がレコード作成と同一トランザクションで、片方だけ残らない
  - [ ] 再配送で副作用が二重に起きない

### Task 13.15: ノートコレクションと静的生成
- **area**: site / **path**: `crates/site`, `web/`
- **仕様**: site フォルダ配下のノート群をコレクションとして登録し、frontmatter の
  `status`/`date`/`slug`/`category`（`NoteMeta` の任意 kv をそのまま使用）で公開対象を決める。
  publish 時にテンプレート `.page` を当てて記事 HTML・一覧・ページネーション・カテゴリ絞り込みを
  **ビルド時に生成**する。サイト内検索は静的 index JSON をバンドルへ同梱する。
- **受け入れ条件**:
  - [ ] `status: published` のノートだけが記事として生成される
  - [ ] 一覧・ページネーション・カテゴリ絞り込みが静的ファイルとして出力される
  - [ ] 下書き（`status` 未設定）のノートが公開バンドルに一切含まれない

### Task 13.16: アニメーション CDN
- **area**: frontend / **path**: `web/`, `crates/app-gateway`
- **仕様**: シキ製の薄い CSS アニメーションライブラリ（`fade-up`・`stagger` 等をクラス付与で効かせる）を作り、
  `builtin` 配信を一般化した**バージョン付き自前 CDN**（`/cdn/shiki-anim@1/...`・immutable）から配る。
  スクロール連動は小さな IntersectionObserver フォールバックを同梱する。
  `prefers-reduced-motion` での一括停止を `globals.css` と同型で必ず入れる。
- **受け入れ条件**:
  - [ ] クラス付与だけでアニメーションが効く（視覚確認・動画）
  - [ ] `prefers-reduced-motion: reduce` で全アニメーションが停止する
  - [ ] バージョン付き URL が immutable キャッシュで配信され、旧版が壊れない

### Task 13.17: A/B テスト
- **area**: api / **path**: `crates/app-gateway`, `crates/site`
- **仕様**: バリアント割当は配信リスナ側で行うが **cookie は焼かない** — 日次ソルト付きハッシュ（IP＋UA）で
  決定的に算出する（同意取得を要さない構成を既定にする）。A/B を有効にしたページはキャッシュ戦略を分け、
  バリアントを跨いだキャッシュ汚染が起きないようにする。
- **受け入れ条件**:
  - [ ] 同一 IP/UA が同日中は同じバリアントに割り当たる
  - [ ] cookie が発行されない
  - [ ] バリアント間でキャッシュが混ざらない

### Task 13.18: 計測
- **area**: api/frontend / **path**: `crates/site`, `crates/jobq`, `migrations/`, `web/`
- **仕様**: 専用テーブル `site_event`（tenant_id 先頭の複合 PK・`unique(tenant_id, idempotency_key)`。
  `migrations/0013_llm_usage.sql` が手本）を新設する。**outbox は使わない**（`node_id not null` かつ
  配送後に GC されるため分析ストアにならない）。受信は sites リスナのビーコン、集計は jobq 経由の非同期。
  生の IP/UA は保存せず日次ソルト付きハッシュのみ。**計測は既定 OFF**で、集計表示には
  `crates/data` と同型の K 未満セル抑制（PIT-17）を掛ける。
  **ビーコンはフォームと同じ匿名書き込み面**なので 13.12 の境界に載せる（本タスクが 13.12 に依存する理由）:
  site/tenant/org は Host と publication から束縛し、サイズ上限・分散レート制限・クォータ・監査を同じ形で適用する。
  `unique(tenant_id, idempotency_key)` は同じ鍵の再送しか止めないため、
  **鍵を作り変え続ける相手には `site_event` と jobq を埋められる** — 一意制約を防御と数えない。
- **受け入れ条件**:
  - [ ] 既定で計測が無効
  - [ ] 生の IP/UA がテーブルに保存されない
  - [ ] 同一冪等キーの二重計上が起きない
  - [ ] 鍵を変え続ける大量送信がレート制限とクォータで止まり、他サイトの計測に波及しない
  - [ ] Host と一致しない site を指定したビーコンが拒否される
  - [ ] 集計表示で K 未満のセルが抑制される

### Task 13.19: 送信データのエクスポート
- **area**: data / **path**: `crates/data`, `crates/tabular`, `web/`
- **仕様**: `data_table` に溜まったフォーム送信を `csv.write` でドライブへエクスポートする
  （蓄積は `data_table`、CSV はエクスポート先という役割分担・design §4.8.2）。
  フィールドマスク（PIT-19）を通した結果だけを出力する。
- **受け入れ条件**:
  - [ ] エクスポートされた CSV がドライブの新規ファイルとして保存される
  - [ ] マスクされたフィールドが CSV に出力されない

### Task 13.20: 独自ドメイン
- **area**: infra / **path**: `deploy/`, `crates/site`, `crates/app-gateway`
- **仕様**: 顧客所有ドメインでの公開に対応する。CNAME による所有確認 → ACME on-demand で証明書取得 →
  Host ルーティングへ登録。証明書取得の失敗と失効を監視し、失効時は既定サブドメインへ縮退させる。
- **受け入れ条件**:
  - [ ] 所有確認を通らないドメインが登録できない
  - [ ] 独自ドメインで TLS 配信され、既定サブドメインでも引き続き到達できる
  - [ ] 証明書取得失敗が観測でき、配信が無防備にならない
