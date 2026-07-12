# クラウドインフラ費用試算：Cloud Run ベース vs GKE Autopilot ベース

> 作成日: 2026-07-12 / 対象リージョン: **asia-northeast1（東京）**（データレジデンシ要件, `docs/design.md:616`, `docs/requirements.md:276`）
> 通貨: USD（参考 JPY は 1 USD = ¥155 換算）。すべて on-demand（コミット割引なし）・月 = 730 時間で換算。
> 位置づけ: **見積り（estimate）**。正本設計ではない。前提を変えれば金額は動く。原価計算に使う前に §7 の「不確実な単価」を公式で最終確認すること。

---

## 0. エグゼクティブサマリ

- **東京・プライベートアルファ想定（共有プール1環境）での月額インフラ費用**
  - **シナリオA（Cloud Run ベース）**: 約 **$1,800 – $2,100 / 月**（約 ¥28万 – ¥33万）
  - **シナリオB（GKE Autopilot ベース）**: 約 **$1,700 – $2,000 / 月**（約 ¥26万 – ¥31万）
  - 差は **5–8% 程度**。GKE Autopilot が「常時稼働ぶんの計算リソース単価」で約3割安いが、その差は**共有マネージド層（Cloud SQL・Redis）と LLM API に総額が薄められて**縮む。
- **最重要の構造的事実**: 指定された **[Cloud Run サンドボックス](https://cloud.google.com/blog/topics/developers-practitioners/google-cloud-run-sandboxes-are-in-public-preview?hl=en)** は
  **Cloud Run サービスインスタンスの中で起動し、割当済み CPU/メモリを共有するため「追加課金ゼロ」**（公式ブログ明記）。
  一方でこれは **Cloud Run の機能**なので、**GKE ベースを選んでも「サンドボックス実行用の Cloud Run コンポーネント」が必要**になる（＝実質ハイブリッド）。
  → **サンドボックス層は両シナリオでコスト中立**。両案の差は「常時稼働するステートレス基盤をどこで動かすか」だけに帰着する。
- **LLM 推論費用は本試算のインフラ費に含めない**。SaaS 版の推論は外部 API / Vertex の**従量課金（pass-through）**であり（`docs/design.md:98,110`）、
  トークン量次第で月 $100〜$1,000+ と大きく変動する。自前 GPU クラスタはオンプレ/エアギャップ要件由来で、SaaS アルファには不要（`docs/requirements.md:268,284`）。

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
| **シナリオA 合計（東京 補正）** | 計算 ×1.28 ＋ マネージド ×1.19 | **≈ $1,865 / 月**（≈ ¥29万） |

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
| **シナリオB 合計（東京 補正）** | 計算 ×1.28 ＋ マネージド ×1.19 | **≈ $1,775 / 月**（≈ ¥28万） |

---

## 6. 比較と感度分析

### 6.1 総額比較（東京）
| | シナリオA: Cloud Run | シナリオB: GKE Autopilot |
|---|---|---|
| 計算層 | ≈ $1,020/月 | ≈ $885/月 |
| 共通マネージド層 | ≈ $845/月 | ≈ $890/月（NAT 分 +） |
| **合計** | **≈ $1,865/月**（¥29万） | **≈ $1,775/月**（¥28万） |
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

## 付録: 主要な前提の出典
- コンポーネント/トポロジ: `docs/design.md` §2–3, §5（`:19-116`, `:622-652`）
- サンドボックス3ティア（アルファは wasm のみ、Cloud Run サンドボックスは `Sandbox` トレイト クラウド実装に対応）: `docs/design.md` §4.6（`:388-439`）
- テナンシ既定＝プール: `docs/roadmap/parallel-tracks.md` SAAS.5（`:320-331`）
- 推論は外部 API/Vertex（SaaS）／自前 GPU 不要: `docs/design.md:98,110`, `docs/requirements.md:268,284`
- 東京リージョン固定: `docs/design.md:616`, `docs/requirements.md:276`
- **スケール数値は設計に存在せず（受注ごとサイジング）**: `docs/requirements.md:285`, `docs/workflow/engine.md:9`
