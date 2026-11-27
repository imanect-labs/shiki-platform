# 計測の方針・SLO の単一定義・計測の現状

原則・段・アラートの方針は [README.md](./README.md)。

## 1. 計測の方針

### 1.1 どこで測るか

| 場所 | 長所 | 短所 | 使いどころ |
| --- | --- | --- | --- |
| ロードバランサ・BFF | 利用者にいちばん近いサーバ側。手間が少ない | 複数のリクエストにまたがる流れは近似になる | API-1〜4 の主な計測点 |
| shiki-server（アプリ） | 意味の分かる区分（クラス・終わり方）を付けられる | サーバに届かなかったリクエストを数えられない | 各領域の SLO の主な計測点 |
| DB のイベント列（`generation_event`・`run_event`・`schedule_occurrence` 等） | 非同期の流れを事象単位で再構成できる | 集計が遅れる | CHAT・WF・ING |
| 合成監視（プローバ） | 件数の少ない流れ・ネットワークの障害・正しさを測れる | 実利用と違う | §3 |
| クライアント（Web） | 体感にいちばん近い | 遅れて届き、揺れる・同意が要る | WEB-1・CHAT-6・転送速度 |

Google SRE の Art of SLOs の整理による〔業〕。

### 1.2 計り方の決まり

- **閾値をまたいだ時点で数える。** 長い処理の SLI は、終わるまで待たずに、閾値を超えた時点で「悪」を確定させる（例: CHAT-2 は 10s を過ぎた時点で悪）〔業〕。終わってから数えると、長く止まっている間アラートが鳴らない。
- **パーセンタイルではなく閾値の割合で集計する。** 「1 秒以内の件数 ÷ 全件数」はレプリカをまたいで足し合わせられ、エラーバジェットにそのまま使える。パーセンタイルは足し合わせられない〔業〕。ヒストグラムのバケットの境界に、各 SLO の閾値を必ず含める。
- **ラベルの数を抑える。**
  - `tenant_id` はメトリクスのラベルに付けない。テナントの下限（README §6）は、テナント別の日次の件数（良・全）を集計テーブルに持って計算する。
  - ルートはパスそのものではなく、ルート定義のテンプレート（`/threads/{id}`）とクラスで付ける。
  - モデルは論理モデル名（モデルカタログの ID）で付ける。
- **良い事象の系列を 0 で初期化する。** 失敗しか起きていない区分の系列が存在しないと、割合の計算から消えてしまう〔業〕。
- **ベンダーコンソールに出すのは集約値だけ**（割合・件数）。顧客データの本文には触れない（design §4.12）。
- **LLM の計測は llm-gateway で行い、OTel の GenAI の規約に合わせる**: `gen_ai.client.operation.duration`・`gen_ai.client.token.usage`・`gen_ai.client.operation.time_to_first_chunk`。
  規約はまだ安定版ではないので、採用した版を固定する。Langfuse には `completionStartTime` も送り、Langfuse 側でも TTFT を見られるようにする。
- **オンプレのローカル推論**は vLLM の Prometheus メトリクス（`vllm:time_to_first_token_seconds`・`vllm:request_queue_time_seconds`・`vllm:kv_cache_usage_perc` 等）をそのまま取り込む。

## 2. SLO の単一定義

SLO の定義は 1 か所に書き、そこから全部を生成する。手で写した対応物を作らない（CLAUDE.md の「codegen が正」と同じ考え方）。GitLab の metrics catalog が先例〔業〕。

```text
deploy/slo/*.yaml（SLO の定義: SLI の式・目標・期間・段）
  ├─ Prometheus の recording rule（5m〜28d の比率）
  ├─ 多窓の burn rate のアラート（README §4.1・最低件数つき）
  ├─ Grafana のダッシュボード（SLO ごとの達成値・バジェットの残り）
  ├─ ベンダーコンソールの集約 SLO（Task 12.4）
  └─ オンプレの同梱物（顧客が目標を上書きできる形）
```

- 定義の形式は [OpenSLO](https://openslo.com/specification/) を候補にし、ルールの生成は Sloth／Pyrra と同じ方式（比率の recording rule → 多窓の burn rate）にする。道具の採否は実装の Issue で決める。
- **リクエストのクラス**（README §3.2）は `route_table()` のルート定義に持たせ、メトリクスの属性に載せる。クラスの無いルートは CI で落とす。
- 文書（本ディレクトリ）と `deploy/slo/` の数値が食い違わないよう、文書の表から定義を生成するか、定義から文書の表を生成するかを実装時に決める。
- Keycloak は SLO の例を製品に同梱している。shiki も同じようにオンプレへ同梱する〔業〕。

## 3. 合成監視とプローバ

SaaS では専用の合成テナント、オンプレではインストール時に作る監視用の組織で動かす。合成監視の失敗は本物の失敗と同じに数える。

| プローバ | 周期 | 確かめること | 対応する SLO |
| --- | --- | --- | --- |
| API の外形 | 30 秒 | ロードバランサの手前から、ログイン済みの読取 API が成功するか（1 時間で 120 件・呼び出しの最低件数を満たす） | API-1・API-2 |
| ログイン | 5 分 | テスト利用者で OIDC のログインが最後まで通るか | AUTH-1 |
| チャット | 5 分 | 短い質問を送り、最初の進捗・完走・切断後の再接続を確かめる（実際のモデルを使う） | CHAT-1〜6 |
| 検索の正しさ | 15 分 | 既知の文書を上げ、権限のある利用者で見つかり、**権限の無い利用者で見つからない**か | ING-1・ING-3・SRCH-1 |
| 共有の反映 | 15 分 | 共有を付与し、5 秒以内に付与先の検索に出るか。解除し、直後に出ないか | SRCH-3・README §7 |
| ワークフローのカナリア | 5 分 | 定時実行・step・タイマー・再試行を含む小さなワークフローが最後まで動くか（デッドマンスイッチ） | WF-1・WF-2・WF-4・WF-5 |
| イベントの心拍 | 1 分 | ダミーのイベントが各コンシューマに届くか | EV-1・EV-2 |
| 耐久性 | 5 分 | 既知のオブジェクトを書き、チェックサムが一致した状態で読み戻せるか | DRV-2 |
| ファイル転送 | 15 分 | presigned URL で直 PUT・直 GET ができるか | DRV-3 |
| 共同編集 | 15 分 | 合成クライアント 2 台で、編集が 1 秒以内に届くか | COL-1・COL-2 |

- プローバの事象は、実利用の SLI と区別できるようラベルを付ける（実利用の件数の少なさを補うのが目的なので、合算して評価する）。
- どのプローバも、**連続 3 回の失敗**で呼び出す（README §4.2。件数の少ない時間帯の全停止を逃さないため）。
- 合成テナントのデータが顧客の集計（AI 活用分析・使用量）に入らないようにする。

## 4. 計測の現状と穴

2026-11-27 時点。計装は #1349、SLO の前提を崩す挙動の修正は #1382 で進める。

| 対象 | 状態 | 足りないもの |
| --- | --- | --- |
| API-1〜4 | 一部あり | `http.server.requests`／`http.server.duration` の属性が `method` と `status` だけ。ルートのテンプレートとクラスが無い。計測は応答ヘッダが返るまでなので、SSE は「ヘッダまでの短い時間」が混ざる（クラスで分ければ解ける）。BFF の 503・外形監視が無い |
| APP-1・APP-2 | なし | 公開 API ゲートウェイ（:8090）・配信（:8091）には計測もタイムアウトも掛かっていない |
| AUTH-1・AUTH-2 | なし | コールバックの成否・所要時間。Keycloak のイベント指標（既定で無効） |
| DRV-1・DRV-2 | なし | finalize の所要時間（大きさ別）、耐久性プローバ |
| CHAT-1〜3・CHAT-6〜8 | 素材のみ | `generation_event` に必要なイベントはある（`status{running}`・`token`・`thinking`・`tool_call`／`tool_result`・`approval_*`）。SLI への集計が無い。`generation_run` に開始・終了の時刻の列が無い。`tool_call` は引数を受け終えた時点で出るので、ツール呼び出しの開始のイベントが無い |
| CHAT-9 | なし | SSE への書き出しの計測・クライアントからの接続の成否と最初のイベントまでの報告 |
| CHAT-4・CHAT-5 | なし | 出力上限での途中切れ・予算での停止・ループ検出が、すべて `status='done'` になる。失敗の理由は自由文の `last_error` だけ。**終わり方の区分の列**が要る |
| LLM-1・LLM-2 | なし | GenAI のメトリクスが無い。Langfuse にも `completionStartTime` を送っていない。`llm_usage` は成功した生成だけでレイテンシ・エラーを持たない |
| SRCH-1・SRCH-2 | 一部あり | `POST /search` は HTTP の計測に載るがルートが分からない。`doc_search` はツールの中なので HTTP に載らない。post-filter で落ちた割合・pre-filter の縮退の率が無い |
| SRCH-3 | テストのみ | 実 OpenFGA の結合テストで確認しているが、本番で測るプローバが無い |
| SBX-1・SBX-2 | ベンチのみ | 実行の結果の区分・起動時間のメトリクス |
| WF-1 | なし | `step.started` は語彙にあるが出していない。`step.ready` は先行 step の完了で ready になったときだけ出す（run の最初の step・再試行・リース回収では出ない）。**上限・流量制限で待たされたことの記録**も無い（外すのに要る） |
| WF-2 | 素材のみ | `schedule_occurrence`（`scheduled_at`・`created_at`）がある。cron の定義から予定を計算する独立した台帳が無い |
| WF-4 | なし | `fail_reason` の区分（利用者・外部・委譲・エンジン） |
| EV-1・EV-2 | 一部あり | `outbox_delivery.delivered_at` がある。日次の突き合わせ・心拍イベントが無い |
| JOB-1・JOB-2 | 一部あり | 着手可能なジョブの滞留の年齢は DB から取れる（`visible_at <= now()` の `now() − visible_at`）。成功したジョブの行は消えるので、待ち時間の分布は claim の時点でメトリクスに出す必要がある |
| TRG-1・TRG-2 | なし | イベントトリガの関数の成否（今はログだけ）、定時トリガの予定の独立した台帳 |
| ING-1〜4 | 素材のみ | アップロード確定（`node_version.created_at`）と索引済み（`rag_ingest_job.updated_at`）から計算できる。ページ数の列が無い（チャンクのページの最大値で近似できる見込み・未検証）。突き合わせ・正しさのプローバが無い |
| DATA-1〜4 | 一部あり | `data.query.*`（走査効率など）はある。ページ取得の所要時間のクラス別の割合・初回描画の実ユーザー計測が無い |
| COL-1〜3 | なし | 接続の成否・編集の伝搬の時間 |
| OPS-1〜7 | 一部あり | プロビジョニングは監査ログ（`tenant.provision`）から数えられる。バックアップは未実装 |
| WEB-1 | なし | Web に計測が無い（`useReportWebVitals` 未使用） |
| ダッシュボード・アラート | なし | Grafana はデータソースだけ。ルール・ダッシュボードが無い（Task 8.5 の受け入れ条件も未達） |
| ingestion-worker | なし | Python 側にメトリクス・OTel が無い |

## 5. 参考

調査日 2026-11-21〜27。

**SRE 一般**
- Google SRE: [Implementing SLOs](https://sre.google/workbook/implementing-slos/)・[Alerting on SLOs](https://sre.google/workbook/alerting-on-slos/)・[Service Level Objectives](https://sre.google/sre-book/service-level-objectives/)・[Example Error Budget Policy](https://sre.google/workbook/error-budget-policy/)・[Example SLO Document](https://sre.google/workbook/slo-document/)・[Data Processing Pipelines](https://sre.google/workbook/data-processing/)・[Distributed Periodic Scheduling](https://sre.google/sre-book/distributed-periodic-scheduling/)・[Canarying Releases](https://sre.google/workbook/canarying-releases/)・[The Art of SLOs handbook](https://static.googleusercontent.com/media/sre.google/en//static/pdf/art-of-slos-handbook-letter.pdf)・[The Calculus of Service Availability](https://sre.google/static/pdf/calculus_of.pdf)
- [Microsoft: Identify flows](https://learn.microsoft.com/en-us/azure/well-architected/reliability/identify-flows)・[Reliability metrics](https://learn.microsoft.com/en-us/azure/well-architected/reliability/metrics)
- [AWS: Availability with dependencies](https://docs.aws.amazon.com/whitepapers/latest/availability-and-beyond-improving-resilience/availability-with-dependencies.html)・[Recommended alarms](https://docs.aws.amazon.com/AmazonCloudWatch/latest/monitoring/Best_Practice_Recommended_Alarms_AWS_Services.html)
- [GitLab: Rails request SLI](https://docs.gitlab.com/development/application_slis/rails_request/)・[Sidekiq execution SLI](https://docs.gitlab.com/development/application_slis/sidekiq_execution/)・[Error budgets](https://handbook.gitlab.com/handbook/engineering/error-budgets/)・[Reference architectures](https://docs.gitlab.com/administration/reference_architectures/)
- [Kubernetes scalability SLOs](https://github.com/kubernetes/community/blob/master/sig-scalability/slos/slos.md)・[Keycloak service level indicators](https://www.keycloak.org/observability/keycloak-service-level-indicators)・[web.dev: Core Web Vitals thresholds](https://web.dev/articles/defining-core-web-vitals-thresholds)
- [OpenSLO](https://openslo.com/specification/)・[Sloth](https://sloth.dev/introduction/)・[Pyrra](https://github.com/pyrra-dev/pyrra)
- [OneUptime: global SLO hiding small customers](https://oneuptime.com/blog/post/2026-08-29-how-to-stop-a-global-slo-from-hiding-reliability-problems-for-small-customers/markdown)・[Google Cloud: deemed SLIs](https://cloud.google.com/blog/products/management-tools/using-deemed-slis-to-measure-customer-reliability)

**ワークフロー・キュー・パイプライン**
- [Temporal: Worker health](https://docs.temporal.io/cloud/worker-health)・[Worker alerting](https://docs.temporal.io/best-practices/worker-alerting)・[Temporal Cloud SLO](https://docs.temporal.io/cloud/slo)・[Activity timeouts](https://docs.temporal.io/encyclopedia/detecting-activity-failures)
- [Cadence: Monitoring](https://cadenceworkflow.io/docs/operation-guide/monitoring)・[AWS Step Functions metrics](https://docs.aws.amazon.com/step-functions/latest/dg/procedure-cw-metrics.html)・[Airflow Deadline Alerts](https://airflow.apache.org/docs/apache-airflow/stable/howto/deadline-alerts.html)
- [Burrow: consumer lag evaluation](https://github.com/linkedin/Burrow/wiki/Consumer-Lag-Evaluation-Rules)・[EventBridge monitoring](https://docs.aws.amazon.com/eventbridge/latest/userguide/eb-monitoring-events-best-practices.html)・[Prometheus: alerting practices](https://prometheus.io/docs/practices/alerting/)・[Brandur: Postgres queues](https://brandur.org/postgres-queues)

**AI・エージェント・LLM**
- Anthropic: [Demystifying evals for AI agents](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents)・[Building effective agents](https://www.anthropic.com/engineering/building-effective-agents)・[Effective harnesses for long-running agents](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)・[A statistical approach to model evals](https://www.anthropic.com/research/statistical-approach-to-model-evals)・[Clio](https://www.anthropic.com/research/clio)
- [tau-bench（pass^k）](https://arxiv.org/abs/2406.12045)・[Revisiting SLOs in LLM serving](https://arxiv.org/html/2410.14257v2)・[DistServe](https://arxiv.org/abs/2401.09670)・[LLM サービスの障害の実証研究](https://arxiv.org/html/2501.12469v2)
- [MLflow: LLM SLO design](https://mlflow.org/articles/llm-slo-design)・[Google Cloud: AI/ML reliability](https://docs.cloud.google.com/architecture/framework/perspectives/ai-ml/reliability)・[NVIDIA NIM metrics](https://docs.nvidia.com/nim/benchmarking/llm/metrics)・[vLLM metrics](https://docs.vllm.ai/en/latest/design/metrics.html)・[OTel GenAI metrics](https://raw.githubusercontent.com/open-telemetry/semantic-conventions-genai/main/docs/gen-ai/gen-ai-metrics.md)
- [OpenAI: background mode](https://developers.openai.com/api/docs/guides/background)・[Trace grading](https://developers.openai.com/api/docs/guides/trace-grading)・[RAGAS metrics](https://docs.ragas.io/en/stable/concepts/metrics/available_metrics/)・[TruLens RAG triad](https://www.trulens.org/getting_started/core_concepts/rag_triad/)・[XSTest](https://arxiv.org/html/2308.01263v3)

**SLA**
- [Vertex AI 生成 AI](https://cloud.google.com/vertex-ai/generative-ai/sla)・[Microsoft Online Services](https://www.microsoft.com/licensing/docs/view/Service-Level-Agreements-SLA-for-Online-Services)・[Cloud SQL](https://cloud.google.com/sql/sla)・[Memorystore](https://cloud.google.com/memorystore/sla)・[Cloud Storage](https://cloud.google.com/storage/sla)・[GKE](https://cloud.google.com/kubernetes-engine/sla)
