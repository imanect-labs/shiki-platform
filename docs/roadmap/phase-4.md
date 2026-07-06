# Phase 4 — サンドボックス＋コードインタプリタ

> 📝 **方針転換（2026-07-05・#97・design §4.6）**: 既定バックエンドを **wasm ティア（agentos フォーク・
> `crates/sandbox-wasm`・非特権別プロセス）** に変更。**アルファは wasm ティアのみ**で、本ファイルの
> Firecracker（4.3）/gVisor（4.4）/温機プール（4.5）/FUSE マウント（4.9）は **gVisor/FC ティア＝ポストアルファ**に
> 後ろ倒しする。wasm ティアでは仮想FSを StorageService に直結（カーネル FUSE 不要・PIT-4/PIT-22 は該当せず）、
> egress は仮想 net スタックのホスト関数で強制、code_interpreter は **Pyodide**（numpy/pandas/matplotlib）。
> `Sandbox` トレイト（4.1）・orchestrator 骨格（4.2）・ツールRPC（4.7）・リソース制限（4.8）・
> code_interpreter 統合（4.10/4.11）は wasm ティアを対象に実装する。
> wasm ティア固有の注意は [PIT-32〜33](../design-caveats.md)。
>
> 目的: 差別化の核となる**隔離された汎用実行環境**（プリミティブ）を立ち上げる。Firecracker/gVisor を
> `Sandbox` トレイトで抽象化した sandbox-orchestrator を別特権プロセスとして作り、温機プール＋スナップショットで
> 高速起動、egress デフォルト遮断＋allowlist、ホスト↔VM ツールRPC、リソース制限を備える。FUSE で StorageService を
> サンドボックス内 `/workspace` にマウントし、チャットに **code_interpreter ツール**（同基盤の制約インスタンス：
> Python限定・ネット遮断・短命）を載せる。参考実装は E2B（OSS）。
> 完了の定義(DoD): チャットでコード（Python）を実行でき、結果が会話に返る。サンドボックスは温機プールから
> <200ms で起動し、egress はデフォルトで遮断され allowlist のみ通る。FUSE 経由の書込は StorageService の権限/監査/
> 書込イベントを必ず通り、RAG 増分再索引がトリガされる。隔離バックエンドは `Sandbox` トレイトで Firecracker/gVisor を
> 差し替え可能。
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
>   ファイル I/O を実 V8/Pyodide で確認済み。
> - 成果物保存（4.11/4.12 Stage A）: `StorageService.write_file_internal`（内部バイト直書き・presigned 経路と
>   同一不変条件）＋`ArtifactStore` トレイト。code_interpreter が `/workspace` の成果物を発話ユーザー権限で保存し、
>   `AgentEvent::Artifact`→SSE `file_ref`→チャット UI のダウンロードチップとして表示。
> - web ツール: `crates/websearch`（`SearchProvider` トレイト＝Brave/SearXNG/Stub・ホスト側）＋
>   `web_search`/`web_fetch` ツール。web_fetch は **run 限定 dynamic_allow** に取得先ホストのみを載せた
>   短命サンドボックスで取得（リダイレクト非追従 PIT-36・IP/内部ホスト拒否・シークレット非添付）。
>   compose に SearXNG（websearch profile）を追加。
> - ゲストコマンドスイート（4.12 software）: `scripts/build-sandbox-commands.sh` が registry/native を
>   nightly+wasm32-wasip1 でビルドしフラットなコマンドディレクトリにステージ（Docker `commands-builder`
>   ステージ・`SANDBOX__COMMANDS_DIR`）。orchestrator は `spec.software` を検証（PIT-23: 名前検証・
>   未同梱は fail-closed）し、ConfigureVm で **`/__secure_exec/commands/0` に host_dir マウント**して
>   `$PATH` に載せる。ls/cat/grep/echo 等が実際に動き stdout が返る（gated IT で検証）。コマンドは
>   `ReadWrite` tier・cwd=/workspace で直接実行（シェル行は shlex 分割・演算子/パイプは非対応＝
>   brush の PTY 要求が出力経路と競合するため。パイプ対応はポストアルファ）。
>   ※ package.tar 投影では native wasm の kernel 管理 stdio が surface しない（#109 調査で判明）。
> - **残（Docker/CI・ポストアルファ）**: C ポートコマンド（curl/wget・wasi-sdk ビルド）の既定同梱、
>   Playwright e2e（code-interpreter / web-search）。

## タスク一覧

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 4.1 | `Sandbox` トレイト定義＋`sandbox-client` gRPC 契約 | sandbox | 3.3 |
| 4.2 | sandbox-orchestrator スケルトン（gRPC API） | sandbox | 4.1 |
| 4.12 | **wasm バックエンド（agentos フォーク・`crates/sandbox-wasm`・仮想FS→StorageService 直結）**〔アルファ既定〕 | sandbox | 4.2, 1.x |
| 4.3 | Firecracker microVM バックエンド実装 **〔実装済・boot は KVM ホストで gated〕** | sandbox | 4.2 |
| 4.4 | gVisor バックエンド実装 **〔実装済・rootless systrap で実機検証〕** | sandbox | 4.2 |
| 4.5 | 温機プール＋スナップショット高速起動（<200ms）**〔ポストアルファ・FC/gVisor 用。wasm は不要〕** | sandbox | 4.3 |
| 4.6 | egress デフォルト遮断＋allowlist ネットワーク制御（wasm=仮想 net ホスト関数） | sandbox | 4.2 |
| 4.7 | ホスト↔サンドボックス ツールRPC（実行/ファイル転送/結果回収） | sandbox | 4.2 |
| 4.8 | リソース制限（CPU/メモリ/PID/時間）＋安全な強制終了 | sandbox | 4.2 |
| 4.9 | `fuse` 仮想FS：StorageService を `/workspace` にマウント **〔ポストアルファ・FC/gVisor 用。wasm は 4.12 の仮想FSで代替〕** | sandbox | 4.7, 1.x |
| 4.10 | `code_interpreter` ツール（Pyodide 制約インスタンス）＋agent-core 接続 | agent | 4.12, 4.6, 4.7, 3.3 |
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
  - shiki-server 側は `sandbox-client`（orchestrator への gRPC クライアント）だけに依存。orchestrator は**別特権プロセス**
    なので、契約は proto で固定（型契約は Rust→proto を真実とする）。
  - **トレイト境界・spec のフィールド・ポリシを慎重に決める。**
- **受け入れ条件**:
  - [ ] `Sandbox` トレイトと `SandboxSpec` が定義され、proto/gRPC 契約が生成される
  - [ ] shiki-server が `sandbox-client` 経由でのみ orchestrator を呼ぶ構造になっている
  - [ ] バックエンド差し替え（Firecracker/gVisor）が spec で選択できる契約になっている

### Task 4.2: sandbox-orchestrator スケルトン（特権プロセス・gRPC API）
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`
- **依存**: 4.1
- **仕様**:
  - orchestrator を**特権の別プロセス**として起動（compose/k8s に追加、shiki-server とは権限分離）。
  - 4.1 の gRPC 契約を実装する骨格：create/exec/put/get/destroy のディスパッチ、バックエンド抽象 `Sandbox` の
    実装スロット（4.3/4.4 が差す）、OTel 計装、構造化ログ。
  - 参考実装 **E2B（OSS）** の制御層構成を踏まえる（自作は制御層のみ、隔離プリミティブは既製）。
  - **境界・proto は 4.1 で確定済み**（systems-heavy な制御層実装）。
- **受け入れ条件**:
  - [ ] orchestrator が特権プロセスとして compose で起動し gRPC を待ち受ける
  - [ ] ダミーバックエンドで create→exec→destroy の往復が通る
  - [ ] orchestrator の操作が OTel trace に出る

### Task 4.3: Firecracker microVM バックエンド実装
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`
- **依存**: 4.2
- **仕様**:
  - **Firecracker を主バックエンド**として `Sandbox` 実装。KVM 上に microVM を起動し、最小カーネル＋rootfs で
    実行環境を提供。exec/ファイル転送を VM 内に橋渡し（4.7 の RPC と接続）。
  - VM ライフサイクル（起動/停止/破棄/クリーンアップ）と jailer による権限降格を扱う。
  - **VM級隔離（NFR-1）** を満たす。スナップショット作成のフックを 4.5 に提供。
- **受け入れ条件**:
  - [ ] KVM 環境で microVM が起動しコマンドを実行できる
  - [ ] VM 破棄でリソースが確実に解放される（リーク無し）
  - [ ] ホストとの隔離（FS/プロセス/ネットワーク名前空間）が確認できる

### Task 4.4: gVisor バックエンド実装（KVM無し環境向けフォールバック）
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`
- **依存**: 4.2
- **仕様**:
  - **KVM が使えない環境（一部クラウド/オンプレ）向けの副バックエンド**として gVisor（runsc）で `Sandbox` 実装。
  - spec の隔離バックエンド種別で Firecracker と切替。インターフェース（exec/put/get）は 4.3 と同一に揃える。
  - 隔離強度・対応制約の差は spec/能力フラグで表明（呼び出し側は同一API）。
- **受け入れ条件**:
  - [ ] gVisor バックエンドで create→exec→destroy が Firecracker と同一APIで通る
  - [ ] spec でバックエンドを Firecracker/gVisor 切替できる
  - [ ] KVM 非搭載環境で gVisor が選択される（自動 or 設定）

### Task 4.5: 温機プール＋スナップショット高速起動（<200ms）
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`
- **依存**: 4.3
- **仕様**:
  - **温機プール（warm pool）**: 事前起動済みインスタンスを保持し、要求時に払い出し→使用後に破棄/補充。
  - **スナップショット**: ベースイメージ（言語ランタイム込み）のメモリ/FSスナップショットから復元起動。
  - 目標 **コールドパス回避で起動 <200ms**（設計書 4.6）。プールサイズ・補充ポリシは設定可能。
- **受け入れ条件**:
  - [ ] 温機プールからの払い出しで起動が <200ms に収まる（計測あり）
  - [ ] スナップショットからランタイム込みで復元起動できる
  - [ ] プール枯渇時に安全にコールドフォールバックする

### Task 4.6: egress デフォルト遮断＋allowlist ネットワーク制御
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`
- **依存**: 4.2
- **仕様**:
  - **egress はデフォルトで全遮断**（機密データ持ち出し防止・エアギャップ対応、NFR-1/NFR-2）。
  - spec の **allowlist（宛先ホスト/ポート/CIDR）にマッチする通信のみ許可**。code_interpreter は allowlist 空（完全遮断）。
  - VM/サンドボックスのネットワーク名前空間に対しフィルタを適用。ブロック/許可の判定を監査ログに残す。
  - **既存判断「egress デフォルト遮断」を厳守。**
- **受け入れ条件**:
  - [ ] allowlist 未設定のサンドボックスは外部到達が全遮断される
  - [ ] allowlist に載せた宛先のみ到達できる
  - [ ] 遮断/許可イベントが監査に残る

### Task 4.7: ホスト↔VM ツールRPC（実行/ファイル転送/結果回収）
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`
- **依存**: 4.2
- **仕様**:
  - VM/サンドボックス内 **ゲストエージェント ↔ orchestrator のRPCチャネル**（vsock 等、egress を経由しない経路）。
  - コマンド実行（stdout/stderr/exit code をストリーム回収）、ファイルの put/get、生成された成果物の回収を担う。
  - agent-core のツール呼出を VM 内実行に橋渡しする土台（4.10 がこの上に code_interpreter を載せる）。
- **受け入れ条件**:
  - [ ] VM 内コマンドの stdout/stderr/exit code がストリームで回収できる
  - [ ] ホスト↔VM 間でファイルを双方向転送できる
  - [ ] RPC は egress allowlist を経由しない隔離経路で動く

### Task 4.8: リソース制限（CPU/メモリ/PID/時間）＋安全な強制終了
- **area**: sandbox / **path**: `crates/sandbox-orchestrator`
- **依存**: 4.2
- **仕様**:
  - spec のリソース上限（**CPU・メモリ・PID/プロセス数・実行時間（壁時計）**）を各バックエンドに適用。
  - 上限超過・タイムアウト時に**安全に強制終了**しリソースを解放。暴走・無限ループを封じ込める。
  - code_interpreter は厳しめの既定（短命・小メモリ・短タイムアウト）を持つ。
- **受け入れ条件**:
  - [ ] CPU/メモリ/PID/時間上限が実際に効く（超過で停止）
  - [ ] タイムアウトしたサンドボックスが確実に破棄される（残留無し）
  - [ ] 制限超過の理由が呼び出し側に返る

### Task 4.9: `fuse` 仮想FS：StorageService を `/workspace` にマウント
- **area**: sandbox / **path**: `crates/fuse`, `crates/storage`
- **依存**: 4.7, 1.x（StorageService／書込イベント）
- **仕様**:
  - サンドボックス内 `/workspace` に **StorageService を FUSE でマウント**。read/write は裏で StorageService を叩き、
    **権限チェック・監査・書込イベントを必ず通す**（バケット/メタ直アクセス禁止のチョークポイント、設計書 4.2）。
  - **FUSE 書込はストレージ書込イベント経由で RAG 増分再索引をトリガ**する（既存判断、FR-2 と一致）。
  - **API は FUSE 前提**で設計。初版実装は sync 妥協可（後で FUSE に差し替え）。`StorageService` が相手で自己完結。
- **受け入れ条件**:
  - [ ] サンドボックス内 `/workspace` で読み書きすると StorageService 経由になる（権限/監査を通る）
  - [ ] FUSE 書込が書込イベントを発行し RAG 再索引がトリガされる
  - [ ] 権限の無いノードは FUSE からも見えない/書けない

### Task 4.10: `code_interpreter` ツール（制約インスタンス）＋agent-core 接続
- **area**: agent / **path**: `crates/agent-core`, `crates/sandbox-client`
- **依存**: 4.5, 4.6, 4.7, 3.3
- **仕様**:
  - agent-core の `Tool` として **`code_interpreter`** を実装。実体は **同じサンドボックス基盤の制約インスタンス**：
    **Python限定・ネット遮断（egress allowlist 空）・短命（実行して破棄）・厳しめリソース上限**（設計書 4.6）。
  - 入力＝Pythonコード、出力＝stdout/stderr/生成成果物（content blocks の tool_result に変換）。温機プールから高速起動。
  - **破壊/コスト系の扱い**: ツール自動選択ポリシ（Task 3.9）に従い、実行の確認/許可制御を尊重する。
  - 制約インスタンスのため初版は **FUSE マウント無し**（まっさら短命）。永続ワークスペース＋FUSE は Phase 5 の自律エージェント。
- **受け入れ条件**:
  - [ ] LLM が `code_interpreter` を呼ぶと Python が実行され結果が会話に返る
  - [ ] インスタンスはネット遮断・短命で、実行後に破棄される
  - [ ] 実行（コード・結果）が監査/Langfuse に記録される

### Task 4.11: チャットでのコード実行可視化＋成果物のストレージ保存
- **area**: frontend / **path**: `web/`, `crates/api`
- **依存**: 4.10, 3.10
- **仕様**:
  - チャットUIで **code_interpreter のツール呼出/実行/出力を可視化**（実行中インジケータ、stdout/stderr、エラー表示）。
    既存の tool_call/tool_result content block・SSE イベント表示（Task 3.5/3.6/3.10）の枠に載せる。
  - 生成された成果物（ファイル/画像/表など）を **StorageService 経由で保存**し、会話から参照（file_ref ブロック）できる。
  - 保存物はストレージ書込イベントを通り、RAG 再索引対象になる（既存判断と一致）。
- **受け入れ条件**:
  - [ ] チャットでコードを実行すると過程と出力がストリーミング表示される
  - [ ] 実行で生成された成果物がストレージに保存され会話から開ける
  - [ ] エラー（実行失敗/タイムアウト/制限超過）がユーザーに分かる形で表示される
