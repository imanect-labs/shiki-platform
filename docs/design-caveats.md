# 設計上の落とし穴・要注意点（実装前に必ず詰める）

> 本書は設計レビューで露見した「このまま実装すると壊れる／詰まる／主張が嘘になる」箇所を、
> 実装着手前に**意思決定して潰すための注意リスト**として固定する。
> 正本は [設計書](./design.md) / [要件](./requirements.md) / [ROADMAP](./roadmap.md)。本書は設計を変えるものではなく、
> 設計が**暗黙にしている前提を明示し、各 Phase の受け入れ条件に反映させる**ためのもの。
>
> 各項目: **箇所 → リスク → 実装前に決めること → 守る不変条件/受け入れ条件**。
> 重大度 🔴 Critical（前提が崩れると当該 Phase が成立しない）/ 🟠 Major / 🟡 Minor。

---

## 🔴 PIT-1: `authz_tags` 前段フィルタの正体を確定する（permission-aware RAG の心臓）

- **箇所**: design.md §4.3 / phase-2 Task 2.2・2.4・2.7。
- **リスク**: ReBAC の可読性は派生的（直接 viewer / 祖先フォルダ / 部署メンバ / 上位部署…の論理和）。
  これをベクタ検索の集合積フィルタに落とす方式が**未定義**。看板機能の中核アルゴリズムが「authz_tags 対応」の4文字で省略されている。
- **実装前に決めること（二択を明示せよ）**:
  - **(a) ACL展開方式**: chunk に「読める subject 全列挙」を焼く → 部署共有でタグ爆発、再共有で大量再書込。
  - **(b) 権限定義オブジェクト方式（推奨）**: chunk に `folder_id / dept_id` 等の**権限定義オブジェクト**を持たせ、
    検索時に**ユーザーの可読オブジェクト集合を OpenFGA `ListObjects` で算出** → `tag IN (集合)` で絞る。
  - (b) を採るなら **`ListObjects` のカーディナリティ非有界問題**に必ず対処する:
    可読集合を folder/dept 粒度に抑え、**集合が閾値超なら pre-filter を諦め post-filter 全依存に切替える**フォールバックを設計に書く。
- **守る/受け入れ条件**: §4.3 に「採用方式・`ListObjects` 上限・タグ再評価のコストモデル」を明記するまで Task 2.4/2.7 を着手しない。
  「最もベスト」を名乗る前提条件＝本項の解決。

## 🔴 PIT-2: post-filter は top-k を壊す（recall が静かに溶ける）

- **箇所**: phase-2 Task 2.6→2.7 の順序（retrieve top-k → RRF → reranker → OpenFGA post-filter）。
- **リスク**: pre-filter が不完全だと rerank 後に大半が deny され、最終引用件数が非決定的に激減。
  reranker が post-filter の**前**にあり、読めない chunk に計算を浪費。CLAUDE.md「全件取得→フィルタ禁止」とも衝突。
- **実装前に決めること**: **over-fetch 係数＋不足時バックフィル**ループを 2.7 仕様に入れる。
  可能なら認可フィルタを **reranker の前**へ寄せ（pre-filter を信頼できる file 粒度で確定）、post は例外検証のみにする。
- **受け入れ条件**: 「最終引用件数が要求 top-k を下回らない（バックフィルが働く）」を 2.7 に追加。

## 🔴 PIT-3: grant 方向の遅延を検査する（「混入ゼロ」の裏面）

- **箇所**: phase-2 Task 2.7（テストが deny 方向＝権限剥奪のみ）。
- **リスク**: pre-filter タグは非同期更新。**権限付与直後**はタグが追いつかず、対象文書が**エラーも出さず検索に出ない**（under-recall）。
  deny は post-filter が救うが、grant は誰も救わない。「共有したのに出てこない」は典型クレーム。
- **実装前に決めること**: grant 後の**可視化 SLA**（付与→N秒以内に検索に出る）を定義。
- **受け入れ条件**: 2.7 に「共有付与→N秒以内に当該 chunk が検索に現れる」adversarial テストを追加（deny と対で）。

## 🔴 PIT-4: FUSE × StorageService の認可/監査は syscall 粒度では破綻する

- **箇所**: design.md §4.2「FUSE の read/write は裏で StorageService を叩き権限/監査/再索引を必ず通る」/ §4.6。
- **リスク**: FUSE は `stat/open/read(128KB単位)/readdir` を1操作で大量発行。各 syscall に OpenFGA check ＋ 監査 insert を乗せると
  サンドボックス内の `ls`/`grep` が実用不能な遅さになり、監査ログが syscall で溢れる。
  **これは sync/async（§4.2「sync 妥協可」）の問題ではなく、認可・監査の粒度が syscall 単位であること**が原因。
- **実装前に決めること**: マウント時に「このワークスペース部分木への **capability**」を1回発行し、
  以降の syscall は capability 検証のみ（毎回 OpenFGA を叩かない）。監査は syscall ではなく**論理操作（open した node）粒度**で記録。
- **守る**: §4.2/§4.6 に capability モデルを追記。「必ず通る」の意味を「capability 発行時に1回通す」と再定義。

## 🔴 PIT-5: エージェントの read-after-write 一貫性が無い（Phase 5 が動かない）

- **箇所**: design.md §4.2 + phase-2 Task 2.8（再索引「数秒〜分」）+ phase-5。
- **リスク**: エージェントは FUSE 上でファイルを編集しつつ `doc_search` する。書込→**非同期**再索引のため、
  **自分が今書いた内容が索引に未反映で stale を読む**。書込速度 ≫ GPU 索引速度で**キューが永久に追いつかない**シナリオすらある。
  ローカル書込と全体索引の一貫性モデルが存在しない。
- **実装前に決めること**: エージェントのワークスペースを「索引対象」と切り離す。
  エージェントの `doc_search` 系は**ワークスペース直読み（grep/file_read ツール）を別経路**にし、
  ストレージRAG索引は**コミット相当の明示操作時のみ**更新する。
- **守る**: phase-5 設計に「ワークスペース直読み経路」と「索引反映の明示境界」を明記。

---

## 🟠 PIT-6: presigned URL は単一チョークポイントを破る

- **箇所**: phase-1 Task 1.2「presigned URL は StorageService の権限判定を経た発行のみ」。
- **リスク**: presigned URL は**発行後 MinIO を直接叩き、有効期間中 StorageService を素通り**。
  「バケット直アクセス禁止」「必ず StorageService 経由」「全アクセス監査」(NFR-6) と矛盾。剥奪してもURLは生き続け、実バイト取得は無監査。
- **実装前に決めること（どちらかに倒す）**:
  - (a) 全バイトを StorageService がプロキシ（チョークポイント厳守・コスト増）。
  - (b) presigned を許すが **極短TTL＋発行を監査＋剥奪時の失効手段**を設計に明記。
- **守る**: 「両方OK」を排し、Task 1.2 にどちらを採るか書く。

## 🟠 PIT-7: `doc_chunk` を OpenFGA オブジェクトにするとタプル爆発

- **箇所**: design.md §4.1 ReBAC 図の `doc_chunk -> inherits -> file`。
- **リスク**: chunk 単位を authz store のオブジェクトにすると百万オーダのタプルが乗り、PIT-1 の `ListObjects` をさらに悪化。
  post-filter check は **file 粒度で十分**で、chunk タプルは純オーバーヘッド。
- **実装前に決めること**: 認可の最小オブジェクトは **file**。chunk→file 対応は RAG 側メタで持ち、OpenFGA に chunk を入れない。
- **守る**: §4.1 の図から `doc_chunk` 関係を削除（または「論理的継承であり tuple 化しない」と注記）。

## 🟠 PIT-8: 埋め込みモデル更新が「全停止イベント」になる

- **箇所**: design.md §4.3 / phase-2 Task 2.3「version 変更＝全再構築」かつ「混在を検出・拒否」。
- **リスク**: エアギャップ・百万文書で全再埋め込みはローカルGPUで数日。検索品質が崩れる。
  「混在拒否」ガードが**ローリング移行（新旧並走）を禁止**し、ゼロダウンタイム移行の道を自ら塞ぐ。
- **実装前に決めること**: **shadow index への背面再構築＋エイリアス切替**を前提化。
  「混在拒否」ではなく「**インデックス単位で version 固定・検索はエイリアス先**」に変更。
- **守る**: §4.3 に shadow→切替の移行手順を追記。Task 2.3 の「混在拒否」をインデックス単位の制約に緩める。

## 🟠 PIT-9: llm-gateway の内部正規形＝OpenAI互換は核に対して逆効果

- **箇所**: design.md §4.5 / phase-3 Task 3.2「内部正規形＝OpenAI互換、薄いアダプタ」。
- **リスク**: 本製品の核はエージェント（agent-core）で、agentic 用途は **Claude（Opus/Sonnet）主力が自然**。
  OpenAI スキーマ正規形は Anthropic の **tool_use/tool_result ブロック・トップレベル system・extended thinking・
  prompt caching ブレークポイント・citations** を綺麗に乗せられず、アダプタは「薄く」ならない。最良モデルの機能を最小公倍数で削る。
- **実装前に決めること**: 内部正規形を**プロバイダ中立な content-block 表現**（tool_use/thinking/citation を一級市民に）にし、
  OpenAI/Gemini をアダプト側にする。最低でも prompt caching と thinking を正規形に持つ。デフォルトモデルは Claude 前提で書く。
- **守る**: §4.5 の「OpenAI互換正規形」を「中立 content-block 正規形」に改める。

## 🟠 PIT-10: Phase 3「最初のデモ」が最大リスク（fable5 の二段authz）に直結

- **箇所**: roadmap phase-2.md（2.10 が 2.7 必須、phase-3 の 3.4 が 2.10 必須）。roadmap.md の「簡易パース先行」前倒し案が phase-2 DoD に反映されていない。
- **リスク**: 最初の顧客価値が、研究的に一番危ない 2.6/2.7（fable5・正しさクリティカル）でゲートされる。
- **実装前に決めること**: phase-2 に**二段DoD**を正式化する。
  - **Tier-1**: file 粒度の単純権限フィルタ（OpenFGA file check のみ・chunk タグ無し）で 2.10/Phase 3 を通す。
  - **Tier-2**: authz_tags による高速 pre-filter（PIT-1）は後追い最適化。
- **守る**: 初デモを RAG 高速化研究から切り離す（Tier-1 で M2 到達可能にする）。

---

## 🟡 PIT-11: OpenFGA の整合性モードを明示する

- **箇所**: phase-1 Task 1.6「共有解除で即時アクセス不可」。
- **リスク**: OpenFGA の書込後 check が結果整合だと「即時」は満たせない。
- **決めること**: 剥奪が即時に効くべき経路は consistency=HIGHER_CONSISTENCY を使う方針を書く（レイテンシ代償も明記）。

## 🟡 PIT-12: 監査ログ「append-only テーブル＝改竄耐性」は過大主張

- **箇所**: phase-1 Task 1.9。
- **リスク**: アプリ層 append-only は DBA／侵害アプリに無力。NFR-6 を名乗るには不足。
- **決めること**: ハッシュチェーン or WORM/外部アンカを採るか、主張を「アプリ経路では追記のみ」と正直に弱める。

## 🟡 PIT-13: フォルダ子一覧「ページング」かつ「権限フィルタ済み」は両立困難

- **箇所**: phase-1 Task 1.5。
- **リスク**: 外部 authz フィルタ＋DB ページングは、オーバーフェッチ無しに正しいページサイズを返せない（PIT-1 の縮小版）。
- **決めること**: 可読集合を先に確定して DB 側で `IN` 絞り込み、またはオーバーフェッチ＋カーソルの方式を Task 1.5 に明記。

## 🟡 PIT-14: content-addressing の dedup 側チャネル

- **箇所**: phase-1 Task 1.2（sha256 でグローバル重複排除）。
- **リスク**: 同一デプロイ内で org 跨ぎ dedup すると hash 存在オラクル（他 org のファイル所持確認）。共有 blob の削除で他 org の参照が壊れる。
- **決めること**: **org スコープ込みの blob 名前空間**＋**refcount による GC** を明記。

## 🟡 PIT-15: Phase 0 が「歩く骨格」にしては重い／SaaS を最初に固定化している

- **箇所**: phase-0 Task 0.8（Grafana 一式）・0.10（skillex SaaS トークン設計）。
- **リスク**: DoD は `/me`＋1トレースなのに観測スタック一式を同梱。SaaS は req §6 で「将来オプション」なのに 0.10 が Phase 0 にあり、
  最も不確実な org 境界を最初に固定化する。
- **決めること**: 0.8 は最小（trace 1本が見える所まで）に絞り、0.10 は認証が安定してからの並行トラックへ後ろ倒し可と明記。

## 🟡 PIT-16: closure table の同時 move 整合

- **箇所**: phase-1 Task 1.1/1.5。
- **リスク**: 並行移動時のロック戦略が無い（循環拒否だけでは不十分・closure 不整合の余地）。
- **決めること**: move を祖先ロック下の単一トランザクションで行う方針を Task 1.5 に明記。

---

## 未レビュー領域（次に詰める）

本書はコア経路（認可/ストレージ/RAG・design §4.1–4.3・phase-0〜3）を対象にした。以下は未精査で、同様の落とし穴が潜む可能性が高い:

- **§4.10 / Phase 9 `data` 行レベル ABAC 述語エンジン**（集計リーク・bypass。fable5 委譲の最高リスク）。
- **§4.6 / Phase 4 サンドボックス**（温機プール・egress・FUSE 特権境界）。
- **§4.1.1 skillex マルチサービス境界**（token-exchange・confused-deputy）。
