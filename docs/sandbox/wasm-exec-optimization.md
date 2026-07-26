# wasm ティア実行高速化: Pyodide ヒープスナップショット

> **背景**: agentos（secure-exec）の wasm サンドボックスは「起動が高速」と謳われるが、
> [bench](./bench.md) の実測で **create ~12ms に対し Python exec が ~6.5s** と判明した
> （exec ごとに Pyodide＝CPython-on-WASM を初期化するため）。起動の軽さ（~12ms・~21MB）は
> 温存したまま、実行側の初期化コストを構造的に潰すのが本機構の目的。

## 問題の分解

wasm ティアの Python exec 1 回のコスト内訳（支配項順）:

1. **`loadPyodide()`**: CPython インタプリタの WASM 上ブートストラップ（stdlib 展開・site 初期化・
   型テーブル構築）。**exec 時間の大半**。
2. pyodide.asm.wasm（~8.6MB）の V8 コンパイル。
3. ランナーモジュール（python-runner.mjs）の import・micropip ロード・各種 shim インストール。

create が軽いのは V8 アイソレート生成が軽いから。重いのは「Python という言語ランタイムの毎回ブート」であり、
これは **メモリスナップショット**（初期化完了後の WASM 線形メモリを保存し、以後は復元でブートを丸ごとスキップ）
で除去できる。Cloudflare Python Workers が同じ手法（Pyodide memory snapshot）でコールドスタートを
削減しており、Pyodide 0.28 系には実験的 API（`loadPyodide({_makeSnapshot})` / `makeMemorySnapshot()` /
`_loadSnapshot`）が入っている。Wizer（wasm の pre-initialization）と同系の発想を Pyodide 公式 API で行う形。

## 実装（vendor/secure-exec: `crates/execution`）

パッチ実体は `vendor/secure-exec/patches/0001-pyodide-heap-snapshot.patch`（fork-policy 準拠）。

### ライフサイクル

```
初回 exec（アセット指紋ごとに 1 回だけ）
  prewarm 起動 ── loadPyodide({_makeSnapshot:true})
              └─ makeMemorySnapshot() → チャンク書き出し（ビルド用ディレクトリ）
  ホストが検証後、クロスプロセスストアへ atomic rename で昇格
以後の全 exec（全 VM・全 sidecar プロセス横断）
  ストア → per-VM パッケージキャッシュへハードリンク
  ランナーが chunk を読み loadPyodide({_loadSnapshot}) で復元（フルブート回避）
```

- **ストア**: `/*temp*/agentos-pyodide-heap-snapshots-v1-uid<euid>/<fingerprint>/`。
  キーは「同梱 Pyodide アセットのビルド時内容ハッシュ（materialize 後の inode/mtime に依存しない）＋
  クレート版＋スナップショット形式版」。カスタム dist はファイル指紋（size/mtime）でキー化。
  `AGENTOS_PYTHON_SNAPSHOT_STORE` で位置を上書き可能（オーケストレータ管理のディレクトリを差せる）。
- **チャンク分割**: 8MiB/チャンク＋末尾 meta.json。Python sync-RPC のデータ上限（20MiB・base64 込み）内で
  ゲスト管理ルート（`/__agentos_pyodide_cache`）経由の read/write に収める＝**ゲスト可視のパス面を拡張しない**。
- **fail-open**: 復元失敗・作成失敗・API 非搭載ビルドはすべて従来のフルブートへフォールバック。
  作成不能なアセットには `.unsupported` マーカーを置き、exec ごとの再試行（毎回 ~4s）を防ぐ。
- **kill switch**: `AGENTOS_PYTHON_SNAPSHOT=0`（exec 要求 env またはプロセス env）。
- **観測**: `AGENTOS_PYTHON_WARMUP_DEBUG=1` で `phase:"snapshot"`（host 側 created/reused/unavailable/disabled）と
  `phase:"startup"` の `snapshot`（restored/off/…）・`snapshotMs`（チャンク読込時間）が出る。

### セキュリティ設計

- **ストア汚染防御**: 共有 temp 配下のストアは euid 所有検証（エントリ dir と meta の両方）を通った場合のみ信頼。
  他 UID が置いたスナップショットは「存在しない」扱い。ストア自体は 0700 で作成。
- **スナップショットに乗るのは信頼済みコードのみ**: 作成はゲストコード実行前の prewarm（ホスト作者のランナー）で行い、
  ゲスト由来バイトは一切含まれない。ゲストへの搬入もハードリンク（read パスは既存の管理ルート confinement のまま）。
- **既知のトレードオフ**: CPython のハッシュ乱択シードがスナップショット作成時のもので固定される
  （全 exec 同一シード）。Cloudflare も同条件を受容している既知の性質。wasm ティアの用途
  （web_fetch 級の短命・読み取り専用実行）ではハッシュ洪水攻撃の実害面が小さいが、
  機微用途に広げる際は再評価すること（PIT-23 の敵対的入力前提は不変）。

## 第2段最適化（同日実装）

段階別計測（`stages` メトリクス・恒久化）で復元後の残コスト ~860ms の内訳を特定し、3 点を潰した:

1. **typed バイナリ搬送**: チャンクの sync-RPC（base64）読込をやめ、ホストがストアから直接
   組み立てた払い出しを `__agentOSWasmModuleBytes`（isolate へ直接注入される Uint8Array・
   Python 起動では未使用だったチャネル）で渡す。meta は `AGENTOS_PYTHON_SNAPSHOT_META` env。
   チャンクファイル読込はフォールバックとして残す。**~180ms → ~1ms**。
2. **micropip 込みスナップショット**: `loadPackage()` は hiwire（JS 参照）エントリを残し
   `makeMemorySnapshot()` が直列化を拒否する（"Unexpected hiwire entry"）ため、
   **wheel を純 Python の zipfile で site-packages へ展開**して焼く（FFI 残渣ゼロ）。
   meta の `micropip: true` を見て復元後の `loadPackage(['micropip'])` をスキップ。**~276ms → 0ms**。
3. **stdlib pre-import 同梱**: 復元後 shim（kernel RPC / hardening / blocklist）が import する
   stdlib（urllib.request / socket / subprocess / json / base64 等）を作成時に import して
   `sys.modules` ごと焼く（純 Python のみ・FFI 不使用）。**pySetupMs ~342ms → ~107ms**。

## 実測（2026-07-26・開発コンテナ・release・公開 0.28.0 dist）

pin 済み dev ビルドのアセットは本環境から取得不能のため、自己完結の公開 v0.28.0 dist を
`SECURE_EXEC_TEST_PYODIDE_DIST` で差して計測（機構はアセット非依存）。

| 経路 | Python ランタイム起動全体（startupMs） | 内訳 |
|---|---|---|
| フルブート（従来） | **~2,340ms** | loadPyodide ~1,830 + micropip ~178 + shim ~316 |
| スナップショット復元（第2段後） | **~562ms（~4.2×）** | loadPyodide ~436 + shim ~107 + 搬送 ~1 + fsShim ~9 |
| スナップショット作成 | ~4,200ms | アセット指紋ごとに 1 回だけ（prewarm 内） |

スナップショットサイズ ~21MB（micropip・pre-import 込み）。残る支配項は復元時 `loadPyodide` の
~436ms（≒ pyodide.asm.wasm 8.6MB の V8 コンパイル＋ヒープ復元 memcpy）。bench.md の exec ~6.5s
環境なら支配項がこの比率で縮む見込み。**正式な 3 ティア比較は dev ホストで `SANDBOX_BENCH=1` の
再計測で更新すること**。

検証: `crates/execution/tests/python_snapshot.rs`（作成→復元→再利用、typed 搬送、micropip スキップ、
無効化フォールバック、**復元後のランタイム状態パリティ**＝cwd/env/argv/version/例外挙動がフルブートと一致）。

## ロードマップ（次の削り代）

1. **pyodide.asm.wasm の V8 コンパイル共有**（残 ~436ms の主部）: rusty_v8 130 の
   `CompiledWasmModule`（Send+Sync）＋`WasmModuleObject::from_compiled_module` は
   **同一プロセス内の isolate 間共有のみ**で、ディスク直列化 API は公開されていない（調査済み）。
   per-sandbox=per-sidecar プロセスの現構成では同一 VM の 2 回目以降にしか効かないため、
   効かせるには (a) session.rs にプロセス共有モジュール注入を実装（同一 VM 連続 exec 向け）、
   (b) rusty_v8 への serialize パッチ（クロスプロセス・上流 PR 要検討）のいずれか。
2. **preload パッケージ（numpy/pandas）込みスナップショット**: ネイティブ拡張（dylink の
   WebAssembly.Instance）は線形メモリ外の JS 状態を持つため現行 API では焼けない。
   preload 構成をキーに含めた多段指紋＋dylink 対応が必要（Cloudflare の package snapshot 相当）。
3. **web_fetch の native 経路**: urllib(Python) を経ない fetch 実装（design §4.6 の既知課題）。
   スナップショットで Python 経路自体が ~0.6s 級まで縮んだため優先度は下がるが、egress モデルは
   不変のままさらに桁を落とせる。
4. **温機プール**: orchestrator 側で復元済み VM をプールすれば体感 create+exec を数十 ms 級へ。
   PIT-22（時間衝突）の再評価が前提。

## 運用ノート

- ストアは temp 配下のキャッシュであり消えても自己修復する（次の exec が再作成）。
- アセット更新（Pyodide 差し替え・クレート版上げ）で指紋が変わり、旧エントリは孤児化する。
  必要ならデプロイ側で temp のクリーンアップに任せる/定期削除する。
- gVisor 既定（design §4.6）は不変。本機構は「wasm を選ぶ理由＝egress モデル」の欠点（Python 初期化税）を
  縮めるものであり、ティア選択の指針は bench 再計測後に見直す。
