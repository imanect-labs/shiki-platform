# Phase 4 — サンドボックス＋コードインタプリタ

> 📝 **方針転換（2026-07-05・#97・design §4.6）**: 既定バックエンドを **wasm ティア（agentos フォーク・
> `crates/sandbox-wasm`・非特権別プロセス）** に変更。**アルファは wasm ティアのみ**で、本ファイルの
> Firecracker（4.3）/gVisor（4.4）/温機プール（4.5）/FUSE マウント（4.9）は **gVisor/FC ティア＝ポストアルファ**に
> 後ろ倒しする。
>
> ⚠️ **この「既定＝wasm」は 2026-07 に撤回された（design §4.6「2026-07 再転換」）**。3 ティア横断ベンチ
> （[bench](../sandbox/bench.md)）で wasm の Python 実行が exec ごとの Pyodide 初期化で ~6s、gVisor の native
> CPython が 82ms と判明したため、**既定は gVisor** に移した。gVisor ティア（4.4）は当初ポストアルファ枠だったが、
> §4.6 で既定へ前倒しし issue #113 として実装済み（design §4.6 のティア表では「アルファ ✅」＝この前倒しを反映）。
> コード既定の反転は #346（rootfs へ numpy/pandas 同梱・runsc 同梱）で完了済み（#346 closed。`SandboxBackend` の
> 既定は `Gvisor`＝`crates/sandbox-client/src/spec.rs`、`chat.sandbox_backend` の既定も gvisor）。
> （旧: 「#346 完了までは `Wasm` のまま据え置く」→ 置換: #346 完了で既定を gVisor へ反転済み）。以下の記述のうち「アルファは wasm のみ」「code_interpreter は Pyodide」は
> 当時の判断であり現行方針ではない（経緯として残す）。wasm ティアでは仮想FSを StorageService に直結（カーネル FUSE 不要・PIT-4/PIT-22 は該当せず）、
> egress は仮想 net スタックのホスト関数で強制、code_interpreter は **Pyodide**（numpy/pandas/matplotlib）。
> `Sandbox` トレイト（4.1）・orchestrator 骨格（4.2）・ツールRPC（4.7）・リソース制限（4.8）・
> code_interpreter 統合（4.10/4.11）は wasm ティアを対象に実装する。
> wasm ティア固有の注意は [PIT-32〜33](../design-caveats.md)。
>
> 目的: 差別化の核となる**隔離された汎用実行環境**（プリミティブ）を立ち上げる。Firecracker/gVisor を
> `Sandbox` トレイトで抽象化した sandbox-orchestrator を別プロセス（当初は特権プロセスの想定・現行は非特権。Task 4.2 参照）として作り、温機プール＋スナップショットで
> 高速起動、egress デフォルト遮断＋allowlist、ホスト↔VM ツールRPC、リソース制限を備える。FUSE で StorageService を
> サンドボックス内 `/workspace` にマウントし、チャットに **code_interpreter ツール**（同基盤の制約インスタンス：
> Python限定・ネット遮断・短命）を載せる。参考実装は E2B（OSS）。
> 完了の定義(DoD): チャットでコード（Python）を実行でき、結果が会話に返る。サンドボックスは温機プールから
> <200ms で起動し、egress はデフォルトで遮断され allowlist のみ通る。FUSE 経由の書込は StorageService の権限/監査/
> 書込イベントを必ず通り、RAG 増分再索引がトリガされる。隔離バックエンドは `Sandbox` トレイトで Firecracker/gVisor を
> 差し替え可能。
> （2026-10 注記: DoD のうち「温機プールから <200ms」は 4.5 がポストアルファのため未実施。gVisor の create p50 は
> 温機プール無しで ~131ms＝[bench](../sandbox/bench.md)。「FUSE 経由の書込」は Phase 5 の Durable Workspace で置換（Task 4.9 参照）。）
>
> **注意**: 本フェーズの **sandbox制御層（Firecracker/gVisor 抽象・温機プール・egress・ツールRPC・リソース制限）**
> と **FUSE 仮想FS** は systems-heavy。トレイト境界・ポリシ
> （egress allowlist、リソース上限、code_interpreter の制約）・チャット統合は慎重に設計する。
>
> ⚠️ **着手前に [設計上の落とし穴](../design-caveats.md) の PIT-4（FUSE の syscall 粒度 authz）・
> PIT-22（高速起動 vs ユーザー束縛の時間衝突）・PIT-23（ゲスト→特権RPC の脱出）・PIT-24（gVisor の隔離低下）・
> PIT-25（egress allowlist の機構）を確認すること。** とくに PIT-23 はホスト侵害に直結する。

> ✅ **実装状況（2026-07・wasm ティア）**: secure-exec を `vendor/secure-exec/` に所有フォークとして取り込み
> （agentos ではなくその依存 secure-exec が実体・[fork-policy](../sandbox/fork-policy.md)）。
> - `crates/sandbox-client`（Task 4.1）: `Sandbox` トレイト＋proto/tonic＋FakeSandbox。
> - `crates/sandbox-orchestrator`（4.2/4.6/4.7/4.8）: 非特権 gRPC・validate（PIT-23）・egress 静的＋動的 allowlist・
>   limits 写像・per-sandbox sidecar 子プロセス（PIT-32）・TTL 掃除。
> - `code_interpreter`（4.10）: agent-core ツール＋ChatWorker 配線。numpy/pandas 利用可・matplotlib 非同梱。
> - gated 実 sidecar 結合テスト（`SANDBOX_IT=1`）: Python 実行・numpy・egress デフォルト遮断・プロセス分離・
>   ファイル I/O を実 V8/Pyodide で確認済み（`SANDBOX_IT` はどの CI ワークフローにも無く、手元実行のみ → #660）。
> - 成果物保存（4.11/4.12 Stage A）: `StorageService.write_file_internal`（内部バイト直書き・presigned 経路と
>   同一不変条件）＋`ArtifactStore` トレイト。code_interpreter が `/workspace` の成果物を発話ユーザー権限で保存し、
>   `AgentEvent::Artifact`→SSE `file_ref`→チャット UI のダウンロードチップとして表示。
> - web ツール: `crates/websearch`（`SearchProvider` トレイト＝Brave/SearXNG/Stub・ホスト側）＋
>   `web_search`/`web_fetch` ツール。compose に SearXNG（websearch profile）を追加。
>   web_fetch は当初 **run 限定 dynamic_allow** の短命サンドボックスで取得していたが、**2026-07（#348）に
>   ホストネイティブ取得へ変更**した（GET 1 回のために Pyodide 初期化 ~6s を払う割に、隔離が守るのは
>   宛先限定だけだったため）。封じ込めはポリシ層で維持: 解決後 IP の再検証＋**検証済みアドレスへの接続固定**
>   （DNS リバインディング遮断・`sandbox_client::net_guard` が IP 分類の単一の正）・リダイレクト非追従
>   （PIT-36）・IP/内部ホスト拒否・シークレット非添付・サイズ上限（design §4.6・PIT-48）。
>   さらに **2026-08（#405）に本文抽出を入れた**: 生 HTML の先頭切りでは本文が 1 文字も入らず
>   トークンだけ焼けていたため、文字コード判定（chardetng）→ ノイズ除去 → Readability 相当の本文特定
>   （dom_smoothie）→ Markdown 化（htmd）→ 自己要約ヘッダ＋`query` 絞り込み／`offset` 続き読みへ変更。
>   PDF/Office は ingestion-worker（Docling）で読むが、**URL ではなくバイト列**を渡す
>   （worker に取りに行かせると宛先制限を迂回する confused deputy になる・PIT-54/PIT-55）。
> - **冪等 read のステップ内並列（#349）**: `web_search`/`web_fetch`/`doc_search` は `Tool::is_read_only()`
>   を表明し、1 ステップ内で有界並列（既定 4・`chat.parallel_read_tools`）に走る。承認要・破壊系は逐次のまま
>   `Approver` を待ち、read はその待ちと並行して進む。観測とイベントは**常に呼び出し順**（PIT-49）。
>   deep research の「検索 → 複数ページ取得」ファンアウトが直列にならないための土台。
> - ゲストコマンドスイート（4.12 software）: `scripts/build-sandbox-commands.sh` が registry/native を
>   nightly+wasm32-wasip1 でビルドしフラットなコマンドディレクトリにステージ（Docker `commands-builder`
>   ステージ・`SANDBOX__COMMANDS_DIR`）。orchestrator は `spec.software` を検証（PIT-23: 名前検証・
>   未同梱は fail-closed）し、ConfigureVm で **`/__secure_exec/commands/0` に host_dir マウント**して
>   `$PATH` に載せる。ls/cat/grep/echo 等が実際に動き stdout が返る（gated IT で検証）。コマンドは
>   `ReadWrite` tier・cwd=/workspace で直接実行（wasm ティアのシェル行は shlex 分割・演算子/パイプは非対応＝
>   brush の PTY 要求が出力経路と競合するため。wasm ティアのパイプ対応はポストアルファ）。
>   gVisor/FC ティアはシェル行をゲストの `/bin/sh -c` が解釈し、パイプ・`&&`・リダイレクトが使える
>   （#504・`gvisor_it::gvisor_shell_line_is_interpreted_by_sh`。shell ツールの description もティアで書き分け）。
>   （旧: 「パイプ対応はポストアルファ」→ 置換: gVisor/FC は #504 で解消、wasm のみポストアルファ）
>   ※ package.tar 投影では native wasm の kernel 管理 stdio が surface しない（#109 調査で判明）。
> - **残（Docker/CI・ポストアルファ）**: C ポートコマンド（curl/wget・wasi-sdk ビルド）の既定同梱、
>   Playwright e2e（code-interpreter / web-search）。e2e は `web/e2e/agent-tools.spec.ts` として実装済みだが、
>   `E2E_AGENT_TOOLS=1`／`E2E_SANDBOX=1` のときだけ走り CI では skip → #662。

> **実装突合（2026-10-17・main 329aba0）**: 既定の gVisor ティアで code_interpreter の create→exec→成果物保存→
> チャットのダウンロードチップまで配線済みで、`Sandbox` トレイト・ツール RPC・egress 既定遮断も実装済み。
> 一方で orchestrator の運用面（計装・認証・同時起動上限・CPU 上限）と、実機テストの CI での実行が欠けている。
> Firecracker（4.3）・温機プール（4.5）はポストアルファ、FUSE（4.9）は Phase 5 の Durable Workspace で置換。
> 残り: OTel 計装・トレース伝播・構造化ログが無い #655／gRPC が無認証・exec 時のテナント照合なし #656／
> 同時起動上限（テナント別・全体）なし #657／gVisor の CPU 上限なし・超過理由が一部しか返らない #658／
> egress 判定が audit_log に残らない #659／実機テストが CI で非ブロッキング・条件不足で skip #660／
> コード実行の可視化が最小限 #661／code_interpreter の E2E が CI で走らない #662／
> 機密度ポリシ（PIT-24）未実装 #663／code_interpreter の実行（コード・結果）が監査・Langfuse に残らない #647

## タスク一覧

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 4.1 | `Sandbox` トレイト定義＋`sandbox-client` gRPC 契約 | sandbox | 3.3 |
| 4.2 | sandbox-orchestrator スケルトン（gRPC API） | sandbox | 4.1 |
| 4.12 | **wasm バックエンド（secure-exec フォーク・`crates/sandbox-orchestrator/src/backend/wasm`）**〔runsc 不在ホスト向けの明示指定ティア。旧: アルファ既定・`crates/sandbox-wasm`・仮想FS→StorageService 直結 → 置換: 既定は gVisor（#346）・wasm backend は orchestrator 内、仮想FS→StorageService 直結は未実装で storage mount 要求は拒否（成果物は shiki-server が回収して `ArtifactStore` で保存）〕 | sandbox | 4.2, 1.x |
| 4.3 | Firecracker microVM バックエンド実装 **〔実装済・boot は KVM ホストで gated・ポストアルファ〕** | sandbox | 4.2 |
| 4.4 | gVisor バックエンド実装 **〔実装済・rootless systrap で実機検証・既定ティア（#346）〕** | sandbox | 4.2 |
| 4.5 | 温機プール＋スナップショット高速起動（<200ms）**〔ポストアルファ・FC/gVisor 用。wasm は不要〕** | sandbox | 4.3 |
| 4.6 | egress デフォルト遮断＋allowlist ネットワーク制御（wasm=仮想 net ホスト関数） | sandbox | 4.2 |
| 4.7 | ホスト↔サンドボックス ツールRPC（実行/ファイル転送/結果回収） | sandbox | 4.2 |
| 4.8 | リソース制限（CPU/メモリ/PID/時間）＋安全な強制終了 | sandbox | 4.2 |
| 4.9 | `fuse` 仮想FS：StorageService を `/workspace` にマウント **〔ポストアルファ・FC/gVisor 用。wasm は 4.12 の仮想FSで代替〕〔置換: Phase 5 の Durable Workspace（phase-5.md 冒頭）〕** | sandbox | 4.7, 1.x |
| 4.10 | `code_interpreter` ツール（選択ティアの制約インスタンス。既定 gVisor＝native CPython／wasm 指定時は Pyodide）＋agent-core 接続 | agent | 4.12, 4.6, 4.7, 3.3 |
| 4.11 | チャットでのコード実行可視化＋成果物のストレージ保存 | frontend | 4.10, 3.10 |

---

## 詳細

### Task 4.1: `Sandbox` トレイト定義＋`sandbox-client` gRPC 契約
- **area**: sandbox / **path**: `crates/sandbox-client`, `crates/sandbox-orchestrator`（型定義）
- **依存**: 3.3
- **仕様**:
  - 可搬性トレイト `Sandbox` を定義（設計書 3.1 の差し替えトレイト群の1本）。最小操作:
    `create(spec) -> handle` / `exec(handle, cmd) -> stream` / `put_file` / `get_file` / `destroy(handle)`。
  - `SandboxSpec` に **隔離バックエンド種別・リソース上限・egress allowlist・FUSEマウント可否・寿命（短命/永続）** を持たせる。
  - shiki-server 側は `sandbox-client`（orchestrator への gRPC クライアント）だけに依存。orchestrator は**別プロセス**（当初は特権の想定・現行は非特権。Task 4.2）
    なので、契約は proto で固定（型契約は Rust→proto を真実とする）。
    （2026-10 注記: 実装は Rust→proto の生成ではない。proto を手書き（`crates/sandbox-client/proto/shiki/sandbox/v1/sandbox.proto`・
    tonic-build で Rust スタブ生成）し、ドメイン型（`spec.rs`）との対応は `convert.rs` の手動変換、往復一致は
    proptest（`crates/sandbox-client/tests/roundtrip.rs`）で担保している。）
  - **トレイト境界・spec のフィールド・ポリシを慎重に決める。**
- **受け入れ条件**:
  - [x] `Sandbox` トレイトと `SandboxSpec` が定義され、proto/gRPC 契約が生成される
  - [x] shiki-server が `sandbox-client` 経由でのみ orchestrator を呼ぶ構造になっている
  - [x] バックエンド差し替え（Firecracker/gVisor）が spec で選択できる契約になっている

### Task 4.2: sandbox-orchestrator スケルトン（非特権の別プロセス・gRPC API）
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`
- **依存**: 4.1
- **仕様**:
  - orchestrator を**別プロセス**として起動（compose/k8s に追加、shiki-server とは権限分離）。
    （旧: 特権の別プロセス → 置換: 非特権プロセス。compose では read_only・no-new-privileges・pids/メモリ制限・
    ストレージ/DB/OpenFGA クレデンシャル無しで起動し、egress 隔離用の user+net namespace は非特権で自前生成する
    ＝`deploy/compose/docker-compose.yml` の sandbox-orchestrator・[fork-policy](../sandbox/fork-policy.md) の blast radius）
  - 4.1 の gRPC 契約を実装する骨格：create/exec/put/get/destroy のディスパッチ、バックエンド抽象 `Sandbox` の
    実装スロット（4.3/4.4 が差す）、OTel 計装、構造化ログ。
  - 参考実装 **E2B（OSS）** の制御層構成を踏まえる（自作は制御層のみ、隔離プリミティブは既製）。
  - **境界・proto は 4.1 で確定済み**（systems-heavy な制御層実装）。
- **受け入れ条件**:
  - [ ] orchestrator が（非特権の）別プロセスとして compose で起動し gRPC を待ち受ける（compose 定義は実装済み・CI では起動されず検証テスト欠落 #662）
  - [x] ダミーバックエンドで create→exec→destroy の往復が通る
  - [ ] orchestrator の操作が OTel trace に出る → #655
  - 補足（2026-10）: gRPC は無認証で、exec 時のテナント照合も無い → #656

### Task 4.3: Firecracker microVM バックエンド実装
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`
- **依存**: 4.2
- **仕様**:
  - **Firecracker を主バックエンド**として `Sandbox` 実装。KVM 上に microVM を起動し、最小カーネル＋rootfs で
    実行環境を提供。exec/ファイル転送を VM 内に橋渡し（4.7 の RPC と接続）。
  - VM ライフサイクル（起動/停止/破棄/クリーンアップ）と jailer による権限降格を扱う。
  - **VM級隔離（NFR-1）** を満たす。スナップショット作成のフックを 4.5 に提供。
  - （2026-10 注記: design §4.6 のティア表で Firecracker は**ポストアルファ**。実装（`backend/firecracker`・vsock の
    ゲストエージェント）はあるが、jailer による権限降格とスナップショットのフックは未実装。egress 要求は
    `Unimplemented` で拒否する（PIT-25 の FC 分は未クローズ）。）
- **受け入れ条件**:
  - [ ] KVM 環境で microVM が起動しコマンドを実行できる（実装済み・`firecracker_it` は `SANDBOX_FC_IT=1` の KVM ホスト限定で CI 外＝検証テスト欠落 #660）
  - [ ] VM 破棄でリソースが確実に解放される（リーク無し）（実装済み・同上 #660）
  - [ ] ホストとの隔離（FS/プロセス/ネットワーク名前空間）が確認できる（実装済み・同上 #660）

### Task 4.4: gVisor バックエンド実装（KVM無し環境向けフォールバック）
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`
- **依存**: 4.2
- **仕様**:
  - **KVM が使えない環境（一部クラウド/オンプレ）向けの副バックエンド**として gVisor（runsc）で `Sandbox` 実装。
  - spec の隔離バックエンド種別で Firecracker と切替。インターフェース（exec/put/get）は 4.3 と同一に揃える。
  - 隔離強度・対応制約の差は spec/能力フラグで表明（呼び出し側は同一API）。
    （2026-10 注記: `SandboxBackend::isolation_class()` で隔離クラスを表明し create 監査に記録するが、
    機密度に応じて gVisor を拒否/警告するポリシ（PIT-24）は未実装＝`validate::check_isolation` は常に許可 → #663）
  - （2026-10 注記: 「副バックエンド」は旧位置づけ。現行は**既定ティア**（#346・design §4.6）。）
- **受け入れ条件**:
  - [x] gVisor バックエンドで create→exec→destroy が Firecracker と同一APIで通る（`gvisor_it`。ただし CI ジョブは continue-on-error の非ブロッキング #660）
  - [x] spec でバックエンドを Firecracker/gVisor 切替できる
  - [x] KVM 非搭載環境で gVisor が選択される（自動 or 設定）（設定 `chat.sandbox_backend`・既定 gvisor。自動判定ではない）

### Task 4.5: 温機プール＋スナップショット高速起動（<200ms）
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`
- **依存**: 4.3
- **仕様**:
  - **温機プール（warm pool）**: 事前起動済みインスタンスを保持し、要求時に払い出し→使用後に破棄/補充。
  - **スナップショット**: ベースイメージ（言語ランタイム込み）のメモリ/FSスナップショットから復元起動。
  - 目標 **コールドパス回避で起動 <200ms**（設計書 4.6）。プールサイズ・補充ポリシは設定可能。
- **受け入れ条件**:
  - [ ] 温機プールからの払い出しで起動が <200ms に収まる（計測あり）（ポストアルファ・未実装。gVisor の create p50 は温機プール無しで ~131ms＝[bench](../sandbox/bench.md)）
  - [ ] スナップショットからランタイム込みで復元起動できる（ポストアルファ・未実装）
  - [ ] プール枯渇時に安全にコールドフォールバックする（ポストアルファ・未実装）

### Task 4.6: egress デフォルト遮断＋allowlist ネットワーク制御
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`
- **依存**: 4.2
- **仕様**:
  - **egress はデフォルトで全遮断**（機密データ持ち出し防止・エアギャップ対応、NFR-1/NFR-2）。
  - spec の **allowlist（宛先ホスト名/ポート）にマッチする通信のみ許可**。code_interpreter は allowlist 空（完全遮断）。
    （旧: 宛先ホスト/ポート/CIDR → 置換: ホスト名（完全一致/明示ワイルドカード）＋ポートに統一し、gVisor は
    SNI 対応 forward proxy、wasm は仮想 net ホスト関数で強制＝PIT-25。`EgressRule` は `host_pattern`・`port` のみ）
  - VM/サンドボックスのネットワーク名前空間に対しフィルタを適用。ブロック/許可の判定を監査ログに残す。
  - **既存判断「egress デフォルト遮断」を厳守。**
- **受け入れ条件**:
  - [ ] allowlist 未設定のサンドボックスは外部到達が全遮断される（実装済み・gVisor は `--network=none`。既定ティアでの遮断テストが無く、wasm の `sidecar_it` は CI 外＝検証テスト欠落 #660）
  - [ ] allowlist に載せた宛先のみ到達できる（実装済み・`gvisor_egress_allows_listed_blocks_others` は CI で `NETNS_HOLDER_BIN` 未設定のため skip、`egress_it` は CI 外＝検証テスト欠落 #660）
  - [ ] 遮断/許可イベントが監査に残る → #659（gVisor の SNI プロキシが stdout に `target=sandbox_audit` の tracing を出すのみ。audit_log への永続化・wasm ティアの記録・検証テストが無い）

### Task 4.7: ホスト↔VM ツールRPC（実行/ファイル転送/結果回収）
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`
- **依存**: 4.2
- **仕様**:
  - VM/サンドボックス内 **ゲストエージェント ↔ orchestrator のRPCチャネル**（vsock 等、egress を経由しない経路）。
  - コマンド実行（stdout/stderr/exit code をストリーム回収）、ファイルの put/get、生成された成果物の回収を担う。
  - agent-core のツール呼出を VM 内実行に橋渡しする土台（4.10 がこの上に code_interpreter を載せる）。
  - （2026-10 注記: 経路はティアごとに異なる。gVisor＝`runsc exec`＋`/workspace` の host bind、
    wasm＝per-sandbox sidecar 子プロセスとの stdio、Firecracker＝vsock のゲストエージェント（`crates/sandbox-guest-agent`）。
    いずれもネットワークを経由しない。）
- **受け入れ条件**:
  - [x] VM 内コマンドの stdout/stderr/exit code がストリームで回収できる
  - [x] ホスト↔VM 間でファイルを双方向転送できる
  - [x] RPC は egress allowlist を経由しない隔離経路で動く

### Task 4.8: リソース制限（CPU/メモリ/PID/時間）＋安全な強制終了
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`
- **依存**: 4.2
- **仕様**:
  - spec のリソース上限（**CPU・メモリ・PID/プロセス数・実行時間（壁時計）**）を各バックエンドに適用。
  - 上限超過・タイムアウト時に**安全に強制終了**しリソースを解放。暴走・無限ループを封じ込める。
  - code_interpreter は厳しめの既定（短命・小メモリ・短タイムアウト）を持つ。
  - （2026-10 注記: サンドボックスの同時起動上限（テナント別・全体）が無い → #657）
- **受け入れ条件**:
  - [ ] CPU/メモリ/PID/時間上限が実際に効く（超過で停止） → #658（既定の gVisor ティアに CPU 上限が無い。PID・CPU・タイムアウトの gVisor 実機テストも無い）
  - [x] タイムアウトしたサンドボックスが確実に破棄される（残留無し）
  - [ ] 制限超過の理由が呼び出し側に返る → #658（`LimitExceeded` は WallClock・Output のみ。メモリ超過の kill は終了コードしか返らない）

### Task 4.9: `fuse` 仮想FS：StorageService を `/workspace` にマウント
- **area**: sandbox / **path**: `crates/fuse`, `crates/storage`
- **依存**: 4.7, 1.x（StorageService／書込イベント）
- **仕様**:
  - サンドボックス内 `/workspace` に **StorageService を FUSE でマウント**。read/write は裏で StorageService を叩き、
    **権限チェック・監査・書込イベントを必ず通す**（バケット/メタ直アクセス禁止のチョークポイント、設計書 4.2）。
  - **FUSE 書込はストレージ書込イベント経由で RAG 増分再索引をトリガ**する（既存判断、FR-2 と一致）。
  - **API は FUSE 前提**で設計。初版実装は sync 妥協可（後で FUSE に差し替え）。`StorageService` が相手で自己完結。
  - （置換: ライブ FUSE は不採用とし、Phase 5 の **Durable Workspace**（thread ごとの StorageService フォルダを
    `fs_*` ツールで直読み/直書き）に読み替えた＝[phase-5.md](./phase-5.md) 冒頭・design-caveats PIT-5 の解消注記。
    `crates/fuse` は Phase 0 のプレースホルダのまま。FUSE は gVisor/FC ティア用のポストアルファ扱い。）
- **受け入れ条件**:
  - [ ] サンドボックス内 `/workspace` で読み書きすると StorageService 経由になる（権限/監査を通る）（置換: Durable Workspace）
  - [ ] FUSE 書込が書込イベントを発行し RAG 再索引がトリガされる（置換: Durable Workspace）
  - [ ] 権限の無いノードは FUSE からも見えない/書けない（置換: Durable Workspace）

### Task 4.10: `code_interpreter` ツール（制約インスタンス）＋agent-core 接続
- **area**: agent / **path**: `crates/agent-core`, `crates/sandbox-client`
- **依存**: 4.12, 4.6, 4.7, 3.3（旧: 4.5 → タスク一覧の表に合わせて 4.12。温機プール 4.5 はポストアルファ）
- **仕様**:
  - agent-core の `Tool` として **`code_interpreter`** を実装。実体は **同じサンドボックス基盤の制約インスタンス**：
    **Python限定・ネット遮断（egress allowlist 空）・短命（実行して破棄）・厳しめリソース上限**（設計書 4.6）。
    ティアは `chat.sandbox_backend`（既定 gVisor＝native CPython・#346）で選び、wasm 指定時は Pyodide。
  - 入力＝Pythonコード、出力＝stdout/stderr/生成成果物（content blocks の tool_result に変換）。温機プールから高速起動
    （温機プールは 4.5 がポストアルファのため未実施。現行は毎回 create）。
  - **破壊/コスト系の扱い**: ツール自動選択ポリシ（Task 3.9）に従い、実行の確認/許可制御を尊重する。
  - 制約インスタンスのため初版は **FUSE マウント無し**（まっさら短命）。永続ワークスペース＋FUSE は Phase 5 の自律エージェント。
  - **会話の添付は実行前に `/workspace/<ファイル名>` へ seed する**（issue #379）。添付は履歴に
    名前しか現れず、モデルは `/workspace/<name>` を読もうとして毎回 `FileNotFoundError` で 1 ステップを
    空費していた（#352 実機デモ）。件数/サイズ上限つきで、超過・失敗は黙らせず
    「node_id で `csv.query` / `doc_search` を使え」という**行動可能な注記**を観測へ載せる。
- **受け入れ条件**:
  - [x] LLM が `code_interpreter` を呼ぶと Python が実行され結果が会話に返る（ツール単体テスト＋`gvisor_it` で検証。ブラウザ E2E は CI 外 #662）
  - [x] インスタンスはネット遮断・短命で、実行後に破棄される
  - [ ] 実行（コード・結果）が監査/Langfuse に記録される → #647（会話の content block・generation_event には残るが、Langfuse は generation のみ・audit_log にも記録が無い）
  - [x] 添付 CSV を渡した会話で 1 回目から `/workspace/<name>` を読める（#379）

### Task 4.11: チャットでのコード実行可視化＋成果物のストレージ保存
- **area**: frontend / **path**: `web/`, `crates/api`
- **依存**: 4.10, 3.10
- **仕様**:
  - チャットUIで **code_interpreter のツール呼出/実行/出力を可視化**（実行中インジケータ、stdout/stderr、エラー表示）。
    既存の tool_call/tool_result content block・SSE イベント表示（Task 3.5/3.6/3.10）の枠に載せる。
  - 生成された成果物（ファイル/画像/表など）を **StorageService 経由で保存**し、会話から参照（file_ref ブロック）できる。
  - 保存物はストレージ書込イベントを通り、RAG 再索引対象になる（既存判断と一致）。
- **受け入れ条件**:
  - [ ] チャットでコードを実行すると過程と出力がストリーミング表示される → #661（実行中表示はあるが、実行コードは非表示・stdout は終了後に結果の先頭 160 字のみ）
  - [ ] 実行で生成された成果物がストレージに保存され会話から開ける（実装済み・`write_file_internal`＋`file_ref` のダウンロードチップ。UI まで通す検証テスト欠落 #662）
  - [ ] エラー（実行失敗/タイムアウト/制限超過）がユーザーに分かる形で表示される → #661（失敗は赤表示のみで、タイムアウトか制限超過かは結果本文の先頭にしか出ない）
