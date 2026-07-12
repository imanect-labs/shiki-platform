# クラウドインフラ費用試算：Cloud Run ベース vs GKE Autopilot ベース

> 作成日: 2026-07-12 / 対象リージョン: **asia-northeast1（東京）**（データレジデンシ要件, `docs/design.md:616`, `docs/requirements.md:276`）
> 通貨: USD（参考 JPY は 1 USD = ¥161.7 換算・2026-07 実勢）。すべて on-demand（コミット割引なし）・月 = 730 時間で換算。
> 位置づけ: **見積り（estimate）**。正本設計ではない。前提を変えれば金額は動く。原価計算に使う前に §7 の「不確実な単価」を公式で最終確認すること。

---

## 0. エグゼクティブサマリ

- **東京・プライベートアルファ想定（共有プール1環境）での月額インフラ費用**
  - **シナリオA（Cloud Run ベース）**: 約 **$1,800 – $2,100 / 月**（約 ¥29万 – ¥34万）
  - **シナリオB（GKE Autopilot ベース）**: 約 **$1,700 – $2,000 / 月**（約 ¥27万 – ¥32万）
  - 差は **5–8% 程度**。GKE Autopilot が「常時稼働ぶんの計算リソース単価」で約3割安いが、その差は**共有マネージド層（Cloud SQL・Redis）と LLM API に総額が薄められて**縮む。
- **最重要の構造的事実**: 指定された **[Cloud Run サンドボックス](https://cloud.google.com/blog/topics/developers-practitioners/google-cloud-run-sandboxes-are-in-public-preview?hl=en)** は
  **Cloud Run サービスインスタンスの中で起動し、割当済み CPU/メモリを共有するため「追加課金ゼロ」**（公式ブログ明記）。
  一方でこれは **Cloud Run の機能**なので、**GKE ベースを選んでも「サンドボックス実行用の Cloud Run コンポーネント」が必要**になる（＝実質ハイブリッド）。
  → **サンドボックス層は両シナリオでコスト中立**。両案の差は「常時稼働するステートレス基盤をどこで動かすか」だけに帰着する。
- **LLM 推論費用は本試算のインフラ費に含めない**。SaaS 版の推論は外部 API / Vertex の**従量課金（pass-through）**であり（`docs/design.md:98,110`）、
  トークン量次第で月 $100〜$1,000+ と大きく変動する。自前 GPU クラスタはオンプレ/エアギャップ要件由来で、SaaS アルファには不要（`docs/requirements.md:268,284`）。
- **コスト最適化（§8）**: アルファ向けに HA・冗長・過剰サイジングを割り切ると、Cloud Run ベースで **月 $640–690（約 ¥10〜11万）** ——
  上記標準構成の **約 1/3（半額以下）** まで圧縮可能。トレードオフ（単一障害点・コールドスタート）と併記。

> ⚠️ **設計ドキュメントには定量スケール要件（想定ユーザー数・テナント数・QPS・同時実行数・レイテンシ/可用性 SLO・データ量）が一切ない**（横断確認済み）。
> 設計は意図的に「初期値・受注ごとサイジング」に委ねている（`docs/requirements.md:285`, `docs/workflow/engine.md:9`）。
> 本試算の規模前提（§1）は**外部から与えた仮定**であり、金額はこの仮定に線形〜準線形にスケールする。

---

## 1. 前提とスコープ（設計に数値がないため明示）

### 1.1 デプロイ形態
`docs/roadmap/parallel-tracks.md:320-331`（SAAS.5）より、**既定は「全テナント共有プール（`tenant_id` 行分離）」**、
cell（顧客ごと専用データプレーン）は強い隔離が要件の顧客向けの opt-in。
→ アルファは **1つの共有環境**で全テナントを収容するモデルで試算する。cell を選ぶ顧客のコストは §6 で加算モデルを示す。

> 📝 **設計の不整合（human 確認推奨）**: デプロイ図 `docs/design.md:93` は クラウドを「顧客ごと隔離インスタンス」と記すが、
> SAAS.1（#84）／SAAS.5 の本文は「既定はプール、cell は将来オプション」。本試算は後者（プール既定）を採用した。図の注記更新を提案したい。

### 1.2 規模前提（＝外から与えた仮定。設計に数値なし）
| 項目 | 仮定値 | 根拠 |
|---|---|---|
| テナント数 | 約 10（設計パートナー） | プライベートアルファの一般的規模。設計に記載なし |
| 総ユーザー数 | 約 200 | 同上 |
| ピーク同時セッション | 約 20–30 | 同上 |
| ベクタ規模 | 約 10万チャンク | 「小規模は pgvector」を採用可能な水準（`docs/design.md:109`） |

### 1.3 コンポーネント → ホスティング対応
出典: `docs/design.md` §2–3, §5（`:19-116`, `:622-652`）。

**常時稼働ステートレス**（HA のため最小レプリカ2。Collabora のみ1）:

| サービス | 実体 | 1レプリカ | レプリカ | 合計 vCPU | 合計 GiB |
|---|---|---|---|---|---|
| shiki-server | Rust モジュラモノリス（api/chat/agent-core/llm-gateway/storage/rag/authz/workflow/collab/office を同居） | 2 vCPU / 4 GiB | 2 | 4 | 8 |
| web | Next.js（generative UI・ミニアプリ配信） | 1 vCPU / 1 GiB | 2 | 2 | 2 |
| Keycloak | 共有コントロールプレーン AuthN | 1 vCPU / 1.5 GiB | 2 | 2 | 3 |
| OpenFGA | ReBAC 認可（GCP マネージドなし＝自前） | 1 vCPU / 1 GiB | 2 | 2 | 2 |
| Collabora Online (CODE) | Office 共同編集 | 2 vCPU / 2 GiB | 1 | 2 | 2 |
| **常時稼働 合計** | | | | **12** | **17** |

**バースト/キュー駆動**:

| サービス | 1インスタンス | 起動特性 |
|---|---|---|
| ingestion-worker | 2 vCPU / 4 GiB | Python/Docling。書込イベント駆動・間欠（稼働率 ~10% と仮定） |
| サンドボックス実行ホスト | 2 vCPU / 4 GiB | **Cloud Run サンドボックス**。リクエスト駆動・最低1インスタンス温存 |

**マネージド・ステートフル（両シナリオ共通）**:
Cloud SQL for PostgreSQL（pgvector 同居）4 vCPU / 16 GB / 200 GB SSD / HA、Memorystore for Redis 4 GB Standard(HA)、
GCS 200 GB、Cloud Load Balancing 1式、Cloud NAT、Artifact Registry。

**本試算のスコープ外**: LLM 推論（外部 API/Vertex 従量・別枠）、自前 GPU（SaaS アルファ不要）、Langfuse/Grafana 等の可観測性自前ホスト分（Cloud Monitoring/Logging 利用を仮定し軽微計上）。

---

## 2. Cloud Run サンドボックスの扱い（設計上の要）

指定資料（[Cloud Run sandboxes — public preview](https://cloud.google.com/blog/topics/developers-practitioners/google-cloud-run-sandboxes-are-in-public-preview?hl=en)）の要点:

- **実行モデル**: 既存の Cloud Run サービスインスタンス内に、near-instant（デモで 1,000 サンドボックスを平均 500ms で起動）で隔離実行境界を spawn。
- **隔離境界**: ①クレデンシャル隔離（env/メタデータサーバ不可視）②ネットワーク既定 deny（egress は明示許可）③ファイルシステム read-only ＋ 使い捨てメモリオーバレイ。
  → shiki の `Sandbox` トレイト（`docs/design.md:111`）の要件（敵対的入力隔離・PIT-23）と整合。
- **課金**: 公式ブログ引用 —「run **directly on your existing allocated CPU and memory** … **there is no additional cost or premium** to use this feature.」
  → **サンドボックス自体に premium 課金はない**。ホストとなる Cloud Run サービスの CPU/メモリ課金に含まれる。
- **含意（両シナリオ共通コスト）**:
  1. Cloud Run サンドボックスは **Cloud Run 上でのみ動く**。GKE 側からは gRPC/HTTP で呼ぶ「サンドボックスホスト用 Cloud Run サービス」を別立てする必要がある。
  2. shiki の設計では `sandbox-orchestrator` は**元から shiki-server と別プロセス**（`docs/design.md:37`, gRPC）なので、これを Cloud Run に切り出すのは自然。
  3. 結果として**サンドボックス実行コスト（≈$116/月, §3）は両シナリオで同額**。両案の差別化要因にならない。
- ⚠️ **preview 注意**: Cloud Run サンドボックスは **public preview**（本番 SLA 非対象）。アルファ検証には使えるが、GA 前提の可用性保証には含めないこと。

---

## 3. 料金レート（裏どり・確度付き）

公式 `cloud.google.com/*/pricing` は JS レンダリングで本文抽出不可のため、**料金を明示する複数の信頼できる二次情報源のクロスチェックで一致した値**を採用。各行に確度を付す。基準は **us-central1 / Tier 1 / on-demand**。

| # | 項目 | 単価（us-central1） | 確度 | 主な出典 |
|---|---|---|---|---|
| R1 | Cloud Run **instance-based**（CPU 常時割当） | vCPU **$0.0648/hr**（$0.000018/s）, メモリ **$0.0072/hr**（$0.000002/s）, リクエスト課金なし | 高 | [economize](https://www.economize.cloud/resources/gcp/pricing/cloud-run/), [cloudcostkit](https://cloudcostkit.com/guides/gcp-cloud-run-pricing/) |
| R2 | Cloud Run **request-based**（処理中のみ割当） | vCPU **$0.0864/hr**（$0.000024/s）, メモリ **$0.009/hr**（$0.0000025/s）, **$0.40/100万req** | 高 | [cloudchipr](https://cloudchipr.com/blog/cloud-run-pricing), [prosperops](https://www.prosperops.com/blog/google-cloud-run-pricing-and-cost-optimization/) |
| R3 | Cloud Run 無料枠/月 | 180,000 vCPU-s ＋ 360,000 GiB-s ＋ 200万 req | 高 | 同上（4源一致） |
| R4 | **Cloud Run サンドボックス** | **premium なし**（ホスト CPU/メモリに包含） | 高 | [公式ブログ](https://cloud.google.com/blog/topics/developers-practitioners/google-cloud-run-sandboxes-are-in-public-preview?hl=en) |
| R5 | Cloud Run GPU (L4, ゾーン冗長なし) | ≈ **$0.672/hr**（別枠・最低 4 vCPU+16 GiB, 無料枠なし） | 中 | [cloudchipr](https://cloudchipr.com/blog/cloud-run-pricing) |
| G1 | **GKE Autopilot** general-purpose Pod | vCPU **$0.0445/hr**, メモリ **$0.0049225/hr**, ephemeral SSD ≈$0.0000706/GiB-hr | 高（3源一致） | [cloudzero](https://www.cloudzero.com/blog/gke-pricing/), [cloudchipr](https://cloudchipr.com/blog/gke-pricing) |
| G2 | GKE クラスタ管理料 | **$0.10/クラスタ-hr**（≈$72/月）、無料枠 **$74.40/月クレジット** → 1クラスタ実質 **$0** | 高 | 同上 |
| G3 | GKE Autopilot Spot Pod 割引 | オンデマンド比 **60–91% 引き** | 中〜高 | [公式](https://cloud.google.com/kubernetes-engine/pricing) |
| S1 | Cloud SQL PostgreSQL (Enterprise) | vCPU **$0.0413/hr**, メモリ **$0.007/GB-hr**, **HA ×2** | 中（下記注） | [usage.ai](https://www.usage.ai/blogs/gcp/cloud-sql/pricing/), [bytebase](https://www.bytebase.com/blog/understanding-google-cloud-sql-pricing/) |
| S2 | Cloud SQL SSD ストレージ | **$0.17–0.222/GB-月**（HA/リージョナルは ×2） | 中（要確認） | 同上 / [NetApp](https://www.netapp.com/blog/gcp-cvo-blg-google-cloud-sql-pricing-and-limits-a-cheat-sheet/) |
| M1 | Memorystore for Redis (Standard/HA, 1–4 GB) | **$0.054/GB-hr** → 4 GB で **≈$158/月** | 高 | [Upstash](https://upstash.com/blog/redis-pricing-comparison-every-major-provider-in-2026-with-numbers) |
| O1 | GCS Standard（リージョン） | **$0.020/GB-月**、Class A **$0.05/万**、Class B **$0.004/万** | 高 | [LeanOps](https://leanopstech.com/blog/google-cloud-storage-pricing-2026/) |
| N1 | Cloud Load Balancing（global ALB） | 転送ルール5個まで **$0.025/hr**（≈$18/月）＋処理 **$0.008/GiB** | 高 | [公式](https://cloud.google.com/load-balancing/pricing) |
| N2 | Cloud NAT | **$0.0014/VM-hr**（上限 $0.044/hr）＋データ処理 **$0.045/GiB** | 高 | [公式](https://cloud.google.com/nat/pricing) |
| N3 | インターネット下り egress (Premium, 0–1TB) | **$0.12/GB**（1–10TB $0.11, 10TB超 $0.08） | 高 | [公式 network-pricing](https://cloud.google.com/vpc/network-pricing) |
| N4 | Artifact Registry | **$0.10/GB-月**（最初 0.5 GB 無料） | 高 | [公式](https://cloud.google.com/artifact-registry/pricing) |
| T1 | **東京(asia-northeast1) プレミアム** | 計算リソース **≈ +28%**（実測: E2-standard-2 $0.067→$0.086, N2-standard-2 $0.097→$0.125）、マネージド ≈ +15–20% | 中 | [gcloud-compute.com](https://gcloud-compute.com/asia-northeast1.html) |

> **重要な確度注記（裏どりの限界）**:
> ・Cloud SQL の vCPU 単価はソース間で **$0.0413〜0.0706** と乖離（採用は内部整合の取れる $0.0413）。SSD は **$0.17 か $0.222** で割れ。→ Cloud SQL は幅を持たせて計上し、**公式ページ/Pricing Calculator での最終確認を強く推奨**。
> ・Cloud Run GPU L4 秒単価は単一ソース依存（本試算では GPU 不使用のため影響なし）。
> ・東京プレミアムは Compute Engine 実測から算出した近似。個別サービスの東京単価は公式で最終確認が望ましい。

---

## 4. シナリオA：Cloud Run ベース

**方針**: 全ステートレスサービスを Cloud Run サービスとしてデプロイ。常時稼働は **instance-based 課金＋min-instances 固定**（アイドルも課金されるが単価が安い R1）。バーストは **request-based＋scale-to-zero**（R2）。サンドボックスは Cloud Run ネイティブ（R4, premium なし）。

| 項目 | 計算 | 月額(us-central1) |
|---|---|---|
| 常時稼働 12 vCPU（instance-based） | 12 × 730 × $0.0648 | $567.6 |
| 常時稼働 17 GiB（instance-based） | 17 × 730 × $0.0072 | $89.4 |
| ingestion-worker（request-based, ~72h/月 実効） | 2×72×$0.0864 + 4×72×$0.009 | $15.0 |
| サンドボックスホスト（instance-based, min=1, 2c/4g） | 2×730×$0.0648 + 4×730×$0.0072 | $115.6 |
| リクエスト課金（~20M − 2M 無料） | 18 × $0.40 | $7.2 |
| **Cloud Run 計算 小計** | | **≈ $795** |
| Cloud SQL 4c/16GB/200GB SSD/HA（Enterprise） | vCPU 4×730×0.0413×2 ＋ mem 16×730×0.007×2 ＋ SSD 200×0.17×2 | $241 + $164 + $68 ≈ **$473** |
| Memorystore Redis 4 GB Standard(HA) | M1 | **$158** |
| GCS 200 GB ＋ オペレーション | O1 | **≈ $8** |
| Cloud Load Balancing | N1 ＋ 処理 ~500GB | **≈ $22** |
| インターネット egress（~400 GB） | N3 | **≈ $48** |
| Artifact Registry | N4 | **≈ $2** |
| **マネージド/共通 小計** | | **≈ $711** |
| **シナリオA 合計（us-central1）** | | **≈ $1,506 / 月** |
| **シナリオA 合計（東京 補正）** | 計算 ×1.28 ＋ マネージド ×1.19 | **≈ $1,865 / 月**（≈ ¥30万） |

Cloud Run では **VPC コネクタ/NAT を使わない構成が可能**（外向き通信は Cloud Run から直接）＝ Cloud NAT を計上せず。

---

## 5. シナリオB：GKE Autopilot ベース

**方針**: 全サービスを 1 つの Autopilot クラスタ上の Pod としてデプロイ（general-purpose クラス G1）。既定でスケールtoゼロしないため、ingestion-worker も最低1レプリカ常駐（KEDA 等でのゼロスケールは運用複雑度と引き換え）。**サンドボックスは §2 の通り Cloud Run を別立て**。

| 項目 | 計算 | 月額(us-central1) |
|---|---|---|
| 常時稼働 12 vCPU（Autopilot GP） | 12 × 730 × $0.0445 | $389.8 |
| 常時稼働 17 GiB（Autopilot GP） | 17 × 730 × $0.0049225 | $61.1 |
| ingestion-worker 常駐（1 レプリカ, 2c/4g） | 2×730×0.0445 + 4×730×0.0049225 | $79.4 |
| Autopilot 最小要求/比率/システム オーバヘッド（~10%） | 概算 | $45 |
| クラスタ管理料 | $72 − $74.40 クレジット | **$0** |
| サンドボックスホスト（**Cloud Run**, §2） | シナリオA と同額 | $115.6 |
| **計算 小計** | | **≈ $691** |
| Cloud SQL / Redis / GCS / LB / egress / AR | シナリオA と同一 | **≈ $711** |
| Cloud NAT（GKE の外向き通信, ~500GB） | N2: gateway ＋ 500×$0.045 | **≈ $35** |
| **シナリオB 合計（us-central1）** | | **≈ $1,437 / 月** |
| **シナリオB 合計（東京 補正）** | 計算 ×1.28 ＋ マネージド ×1.19 | **≈ $1,775 / 月**（≈ ¥29万） |

---

## 6. 比較と感度分析

### 6.1 総額比較（東京）
| | シナリオA: Cloud Run | シナリオB: GKE Autopilot |
|---|---|---|
| 計算層 | ≈ $1,020/月 | ≈ $885/月 |
| 共通マネージド層 | ≈ $845/月 | ≈ $890/月（NAT 分 +） |
| **合計** | **≈ $1,865/月**（¥30万） | **≈ $1,775/月**（¥29万） |
| レンジ | $1,800–2,100 | $1,700–2,000 |

**差 ≈ $90/月（5%）。** GKE Autopilot は常時稼働ぶんの単価が約3割安い（G1 vs R1）が、
①総額の過半が両案共通のマネージド層 ②サンドボックスは両案とも Cloud Run ③GKE は Cloud NAT が乗る ——で差が縮む。
**東京プレミアムは両案にほぼ等しく効く**ため、A–B の差は「東京かどうか」にほぼ不変。

### 6.2 どちらが有利かは「負荷の形」で決まる
- **バースト/スパイクが多い（アルファの実態に近い）→ Cloud Run 有利**。scale-to-zero で idle を払わない。ingestion/サンドボックスが間欠なほど効く。
- **常時高稼働・高密度 → GKE Autopilot 有利**。単価が安く、Spot Pod（G3, 最大91%引き）でバッチ系をさらに圧縮できる。
- **運用コスト（人件費）**: Cloud Run はマネージド度が高くアルファ運用が軽い。GKE は K8s の運用・アップグレード・ネットワーク設計の負担が増える（本試算の金額には未計上だが、アルファ段階では無視できない差）。

### 6.3 主な感度パラメータ
| レバー | 効果 |
|---|---|
| **同時実行数（concurrency）** | Cloud Run は req 課金以外はインスタンス時間課金。concurrency を上げるほど必要インスタンスが減り単価改善。 |
| **min-instances / 常駐レプリカ** | 常時稼働の主コスト。HA を 2→1 に落とせば計算層はほぼ半減（ただし可用性低下）。 |
| **CUD（確約利用割引）** | Cloud SQL は 1年で25%・3年で52%引き。GKE Autopilot/Compute も CUD 対象。アルファ後の定常化で最大効果。 |
| **cell（顧客専用データプレーン）** | opt-in の顧客ごとに **専用 shiki-server ＋ 専用 Cloud SQL（最小 2c/8g/HA ≈ $240）＋ 小型 Redis** が加算。**1 cell あたり概ね +$400–700/月**。cell を多用すると総額は「テナント×フルスタック」で線形増（設計が SAAS.5 でプール既定にした理由）。 |
| **LLM API（別枠）** | 外部 API/Vertex 従量。トークン量で月 $100〜$1,000+。**インフラ費と同等かそれ以上になり得る**主要変動費。実測トークンで別途試算が必要。 |
| **東京 → 他リージョン** | us-central1 なら計算層 ≈ −22%。ただしデータレジデンシ要件（東京固定）に反するため本番は不可。 |

---

## 7. 結論・推奨

1. **純インフラ費では両案はほぼ互角（東京で月 $1,800 前後、差 5–8%）。** 金額だけで決める段階ではない。
2. **アルファには Cloud Run ベース（シナリオA）を推奨。** 理由:
   - サンドボックスが**ネイティブ・追加課金ゼロ**で、`Sandbox` トレイトの隔離要件に合致。GKE では結局 Cloud Run を別立てする必要がある。
   - アルファの負荷はスパイク主体 → **scale-to-zero** の恩恵が大きく、GKE の単価優位を相殺。
   - **運用が軽い**（K8s 運用工数を割かずプロダクト検証に集中できる）。
3. **GKE Autopilot は「定常化・高密度・複数テナント常時稼働」フェーズで再評価。** その段階なら単価優位＋Spot＋CUD で逆転し得る。移行は `LlmProvider`/`ObjectStore` 等のトレイト境界（`docs/design.md:104-116`）を保っていれば段階的に可能。
4. **必ず別枠で見積るもの**: ①LLM API/Vertex トークン費（最大の変動費）②cell を選ぶ顧客ぶんの加算（§6.3）③可観測性を自前ホストする場合の追加。
5. **裏どりの最終確認事項**（§3 の確度「中」）: Cloud SQL の vCPU/SSD 単価と東京プレミアムは、Google Cloud Pricing Calculator / 実 Console の SKU で確定してから予算化すること。

---

## 8. コスト最適化：Lean アルファ構成（目標：半額以下）

§4–5 の標準構成は **HA（冗長2レプリカ）＋余裕を持たせたサイジング**を前提にしている。
プライベートアルファは「本番 SLA を約束しない検証環境」なので、以下を割り切れば **標準 $1,865 → 約 $640–690/月（東京）＝ 約 1/3** に落とせる。
以下は **シナリオA（Cloud Run ベース）** を Lean 化したもの（GKE 版も 1 レプリカ化＋Spot Pod で同水準に落とせるが、Cloud Run の方が単純）。

### 8.1 削減レバー（効果順・東京換算）
| # | レバー | 変更 | 月額削減 |
|---|---|---|---|
| L1 | **Cloud SQL の右サイジング＋HA 撤去** | 4c/16GB/200GB/HA → **2c/8GB/100GB/HA なし**（Enterprise）。10テナント/200ユーザー＋10万チャンクの pgvector なら十分 | **≈ −$424**（$564→$140） |
| L2 | **常時稼働の冗長撤去（2→1 レプリカ）** | shiki-server 以外の HA を外し、warm を **5 vCPU / 7 GiB** に集約 | **≈ −$490**（$841→$350） |
| L3 | **Memorystore を Basic 1GB へ** | Standard 4GB(HA) → **Basic 1GB**（セッション/pub-sub/レート制限に十分） | **≈ −$145**（$188→$43） |
| L4 | **間欠サービスを scale-to-zero** | Collabora・ingestion-worker を min=0（使用時のみ起動） | **≈ −$140** |
| L5 | **Global ALB を撤去** | 標準の `*.run.app` HTTPS ＋ ドメインマッピング（無料）を利用。WAF が要るまで LB 不要 | **≈ −$26** |
| L6 | **egress 抑制** | アルファのトラフィック実測に合わせ縮小 | ≈ −$20 |
| L7（任意） | **CUD 1年コミット** | 定常化した Cloud SQL＋warm Cloud Run に 1年 CUD（−25%）。※アルファでの1年コミットはリスクと相談 | 追加 −$100 前後 |

### 8.2 Lean 構成の内訳（東京・CUD なし）
| 区分 | 構成 | 月額(東京) |
|---|---|---|
| Cloud Run warm（instance-based, min=1） | shiki-server 2c/4g・web 1c/1g・OpenFGA 1c/1g・Keycloak 1c/1g ＝ 計 **5 vCPU / 7 GiB** | ≈ $350 |
| Cloud Run scale-to-zero | Collabora・ingestion（min=0）＋ サンドボックスホスト（min=1, 1c/2g）＋ req 課金 | ≈ $110 |
| Cloud SQL | 2c/8GB/100GB SSD・**HA なし**（Enterprise） | ≈ $140 |
| Memorystore Redis | **Basic 1 GB** | ≈ $43 |
| GCS / egress / Artifact Registry | 実測ベース縮小 | ≈ $42 |
| LB | **なし**（Cloud Run 標準 URL） | $0 |
| **Lean 合計** | | **≈ $685 / 月**（≈ ¥11.1万） |
| サンドボックス min=0（さらに割り切り） | 対話レイテンシと引き換え | **≈ $640 / 月** |
| L7（CUD 1年）併用 | 定常化後 | **≈ $560 / 月** |

→ **標準 $1,865 に対し約 63–70% 減。半額（$930）を明確に下回る。**

### 8.3 割り切りのトレードオフ（正直な注記）
- **単一障害点**: HA/2レプリカ撤去でインスタンス障害・ゾーン障害時にダウン。**本番 SLA には使えない**（アルファ検証専用）。Cloud SQL も HA なしはフェイルオーバー不可・パッチ時に瞬断。
- **コールドスタート（UX 影響）**: scale-to-zero したサービスは初回リクエストで数秒の遅延。
  → **対話パス（shiki-server）は min=1 を維持**して体感を守る。背景系（ingestion）・低頻度（Collabora）のみ min=0 にするのが妥協点。
  サンドボックスホストを min=0 にするとエージェント初回実行が遅くなる（Cloud Run サンドボックス自体は ~500ms だがホスト起動が乗る）。UX 重視なら min=1 のまま（$685 側）を推奨。
- **ヘッドルーム縮小**: 2c/8GB の Cloud SQL・Basic 1GB Redis は同時実行が増えると頭打ち。**監視して閾値で1段上げる**運用前提。負荷が読めたら CUD で単価を戻す。
- **cell を選ぶ顧客**（§6.3）はこの Lean 前提から外れ、専用スタックぶんが加算される。

### 8.4 段階戦略（推奨）
1. **アルファ検証期**: Lean 構成（min=1 は shiki-server ＋ サンドボックスのみ、$685/月）で開始。
2. **利用が読めたら**: 実測 QPS/同時実行でボトルネックのサービスだけ 2 レプリカ化・Cloud SQL 昇格。
3. **定常化・本番化**: HA を戻し、Cloud SQL＋warm 分に **CUD（1年 −25%／3年 −52%）** を適用して単価を回収。標準構成でも CUD で実効 $1,400 前後まで下がる。

---

## 9. フェーズ別費用予測（顧客数スケール）＋ GKE 早期移行

> ⚠️ 前提として **設計にスケール数値は無い**（§0）。以下のフェーズ定義・リソースサイジングは**外から与えた仮定**。
> 金額は仮定に準線形にスケールするため、実測が出たら差し替える運用とする。**LLM API 費（別枠・従量）は含まない**。全額 東京・月額・USD。

### 9.1 フェーズ定義（顧客数で刻む）
| フェーズ | 顧客(テナント) | 総ユーザー | ピーク同時 | 可用性posture | 位置づけ |
|---|---|---|---|---|---|
| **P0 プライベートアルファ** | 〜10 | 〜200 | 〜25 | HAなし（単一レプリカ・Lean §8） | 検証専用 |
| **P1 クローズドβ** | 〜30 | 〜1,000 | 〜100 | HA導入（主要サービス2レプリカ・Cloud SQL HA） | 有償前・SLA準備 |
| **P2 GA / 初期成長** | 〜100 | 〜5,000 | 〜500 | HA＋一部スケールアウト | 本番SLA |
| **P3 スケール** | 〜300 | 〜20,000 | 〜2,000 | 冗長化＋Spot＋CUD | 定常運用 |

### 9.2 リソースサイジング仮定（warm＝常時稼働の総量）
| | 常時 warm | Cloud SQL | Redis | ingestion/バッチ | サンドボックス(Cloud Run) |
|---|---|---|---|---|---|
| P0 | 5 vCPU / 7 GiB | 2c/8GB/100GB・HAなし | Basic 1GB | Spot 2c 間欠 | min=1 小(1c/2g) |
| P1 | 12 vCPU / 17 GiB | 4c/16GB/200GB・HA | Std 4GB HA | Spot 4c | 2c/4g |
| P2 | 21 vCPU / 30 GiB | 8c/32GB/500GB・HA | Std 10GB HA | Spot 8c | スケール |
| P3 | 50 vCPU / 80 GiB | 16c/64GB/1TB・HA(+RR) | 35GB(→Cluster) | Spot 16c | 高負荷 |

### 9.3 フェーズ別 月額予測（GKE Autopilot vs Cloud Run・東京）
GKE は バッチ(ingestion)に **Spot Pod（〜70%引き）** を適用、サンドボックスは両案とも Cloud Run（§2）。

| | **GKE Autopilot** | Cloud Run | 差(GKE有利) | GKE の顧客あたり/月 |
|---|---|---|---|---|
| **P0（10社）** | **≈ $620**（¥10.0万） | ≈ $650 | 〜$30 | **≈ $62/社** |
| **P1（30社）** | **≈ $1,900**（¥30.7万） | ≈ $2,000 | 〜$100 | **≈ $63/社** |
| **P2（100社）** | **≈ $4,350**（¥70.4万） | ≈ $4,650 | 〜$300 | **≈ $44/社** |
| **P3（300社）** | **≈ $10,950**（¥177万） | ≈ $11,900 | 〜$960 | **≈ $37/社** |

主な内訳の推移（GKE・概算）:
| 内訳 | P0 | P1 | P2 | P3 |
|---|---|---|---|---|
| warm 計算（Autopilot GP・+10%oh） | $264 | $635 | $1,112 | $2,692 |
| サンドボックス(Cloud Run) | $60 | $150 | $500 | $1,500 |
| ingestion/Collabora(Spot/pod) | $48 | $153 | $307 | $614 |
| Cloud SQL(HA) | $141 | $562 | $1,165 | $2,331 |
| Memorystore | $43 | $188 | $460 | $1,125 |
| GCS＋egress＋LB＋NAT＋AR | $64 | $212 | $808 | $2,690 |
| クラスタ管理料（$74.4/月クレジットで相殺） | $0 | $0 | $0 | $0 |

- **顧客あたりコストは逓減**（$62 → $37）。プール型（SAAS.5 既定）の規模の経済がそのまま効く。P0→P1 で横ばいなのは HA 導入で一段積むため。
- **P2 以降の3大コスト = Cloud SQL・egress・Redis**（＝プラットフォーム非依存の共通層）。ここが総額を支配するので、**GKE/Cloud Run の選択より、この3つの最適化の方が効く**（§9.6）。

### 9.4 「早めに GKE へ」を支持する根拠
1. **金額ペナルティがほぼ無い**: GKE は全フェーズで Cloud Run と同等〜わずかに安い（上表）。**早く移しても損しない**。移行の障壁は費用ではなく **K8s 運用力**のみ。
2. **移行リスクは今が最小**: マルチテナント稼働中（P2/P3）の基盤移行は高リスク。**顧客・トラフィックが少ない P0〜P1 のうちに移すのが最も安全**。これが「早期」を推す最大の理由。
3. **移行で動くのはステートレス層だけ＝低リスク**: Cloud SQL / Memorystore / GCS（マネージド）と **サンドボックス(Cloud Run)** は移行で不変（§9.5）。実際に載せ替えるのは shiki-server / web / OpenFGA / Keycloak / Collabora の Deployment 化と Gateway/Ingress のみ。トレイト境界（`docs/design.md:104-116`）が保たれているため差分は限定的。
4. **サンドボックス・ロードマップとの整合**: 現状の Cloud Run サンドボックスは wasm 相当ティアのみ。**post-alpha の gVisor / Firecracker（microVM）ティア**（`docs/design.md:388-439`）は KVM/ノード制御が要り、**GKE でしか収容できない**。早期に GKE 基盤を持てば、Cloud Run サンドボックス（当面の指定）→ 将来 gVisor/Firecracker をクラスタ内に取り込む拡張余地ができる。
5. **単価を下げるレバーが GKE 側に多い**: Spot Pod（バッチ最大91%引き）、ノード CUD、ビンパッキング（複数サービス同居）、HPA/VPA。定常化フェーズで効く。

### 9.5 移行で「変わるもの / 変わらないもの」
| 変わらない（移行不要） | 変わる（移行対象） |
|---|---|
| Cloud SQL / Memorystore / GCS（マネージド） | ステートレス各サービスを Cloud Run Service → GKE Deployment 化 |
| **サンドボックス**（Cloud Run 別立てのまま・§2） | 外部公開を `run.app` → Gateway API / Ingress + LB |
| Artifact Registry（同一） | GCP アクセスを Workload Identity 化 |
| トレイト実装（ObjectStore/LlmProvider 等） | 外向き通信に Cloud NAT を追加、HPA・PodDisruptionBudget 設定 |
| LLM 外部API/Vertex（従量・別枠） | 可観測性（Cloud Monitoring 継続 or Grafana をクラスタ内へ） |

→ **データ層とサンドボックスが不変**なので、移行は「ステートレス層の再デプロイ」に収束。ダウンタイムは DNS/LB 切替のみで最小化できる。

### 9.6 スケール時に効く最適化（GKE 選択より効果大）
- **Cloud SQL**: P2 以降は **CUD（1年 −25%／3年 −52%）** を必ず適用。読み取り負荷は **read replica / AlloyDB** 検討で単価改善。pgvector が重くなれば専用 Qdrant へ退避（`VectorStore` トレイト）。
- **egress**: P2 で $560、P3 で $1,910 と無視できない。**Cloud CDN で配信キャッシュ**すれば大幅圧縮。
- **Redis**: 35GB を従来 Memorystore で持つと $1,125/月。**Redis Cluster / Valkey のノード課金**に切り替えると GB 単価が下がる。
- **warm 計算**: ノード CUD ＋ ビンパッキングで実効単価をさらに 20–30% 圧縮可能。

### 9.7 推奨タイムライン
1. **P0（今）**: Lean 構成で検証（Cloud Run $685 or GKE $620）。**チームに K8s 運用力があれば P0 から GKE で開始**（費用は既に同等以下）。
2. **P0 → P1 の境界（β直前）で GKE へ移行**（運用力をこの間に用意）。HA 化と重なるため“どうせ触る”タイミングであり、トラフィック最小で最も安全。
3. **P2 以降**: CUD・Spot・CDN・read replica を適用し単価を回収。GKE の単価優位＋最適化レバーがここで効いてくる。

> **結論**: 「早めに GKE」は費用面で妥当（損しない）かつ **移行安全性の観点ではむしろ推奨**。ただし前提は **P1 までに K8s 運用体制（クラスタ運用・アップグレード・ネットワーク/セキュリティ）を用意**できること。総額を左右するのは最終的に Cloud SQL・egress・Redis・**LLM API** なので、プラットフォーム選定と並行してそこを詰めること。

---

## 付録: 主要な前提の出典
- コンポーネント/トポロジ: `docs/design.md` §2–3, §5（`:19-116`, `:622-652`）
- サンドボックス3ティア（アルファは wasm のみ、Cloud Run サンドボックスは `Sandbox` トレイト クラウド実装に対応）: `docs/design.md` §4.6（`:388-439`）
- テナンシ既定＝プール: `docs/roadmap/parallel-tracks.md` SAAS.5（`:320-331`）
- 推論は外部 API/Vertex（SaaS）／自前 GPU 不要: `docs/design.md:98,110`, `docs/requirements.md:268,284`
- 東京リージョン固定: `docs/design.md:616`, `docs/requirements.md:276`
- **スケール数値は設計に存在せず（受注ごとサイジング）**: `docs/requirements.md:285`, `docs/workflow/engine.md:9`
