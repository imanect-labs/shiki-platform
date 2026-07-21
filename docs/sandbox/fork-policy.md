# secure-exec フォーク運用ポリシ

> PIT-32 の受け入れ条件（`docs/design-caveats.md`）に対応。サンドボックス wasm ティアの実体である
> [secure-exec](https://github.com/rivet-dev/secure-exec) を **我々が所有するフォーク**として保守する方針。

## 位置づけ

- `vendor/secure-exec/` は shiki が**所有するソース**。上流（rivet-dev/secure-exec）の破壊的変更に
  追従する義務は負わない。pin は `vendor/secure-exec/UPSTREAM`（commit SHA）。
- 設計原則4「隔離プリミティブは自作しない」は本件で一部撤回済み（design §4.6）。フォークは Rust 製
  in-process OS カーネルを我々が抱える決断であり、下記の運用で blast radius とサプライチェーンを管理する。

## 上流との関係（任意 cherry-pick）

- 上流追従は**周期義務ではなく必要駆動**。欲しい修正・セキュリティ patch が出たときに cherry-pick する。
- ローカル変更は `vendor/secure-exec/patches/` に**番号付き最小 diff**で置き、`UPSTREAM` に列挙する。
  意味のある改変は上流 PR 化を試みる（例: egress 判定イベント発火 patch）。
- 再 vendor は `scripts/update-secure-exec.sh`（clone → サブセット抽出 → patches 適用 → ビルド確認）。

## 依存 CVE ウォッチ（残る本当のリスク）

フォーク所有により「上流の API 破壊」リスクは消えるが、**依存の脆弱性**は残る。特に:

- **V8（`v8` crate / rusty_v8・130 系 pin）**: 隔離境界そのもの。V8 の安定チャネルセキュリティリリースを
  監視し、rusty_v8 の対応バージョンへ追従する。V8 の 0-day は境界破りに直結する（ブラウザと同じ隔離技術だが
  0-day 負債は同じく抱える）。
- `cargo deny check`（RUSTSEC advisory / license / sources）を **CI 常設**。vendor は shiki workspace から
  exclude されるが、`cargo deny` は依存グラフ全体を走査するため vendor の依存も検査される。
- advisory を一時 ignore する場合は理由と追従条件を `deny.toml` にコメントで残す。

## blast radius（プロセス分離粒度）

- サンドボックスは **per-sandbox の `secure-exec-sidecar` 子プロセス**（1 transport = 1 session = 1 VM）。
  in-process カーネルを shiki-server に同居させない（PIT-32）。
- **sidecar プロセス侵害の想定被害 = そのサンドボックス 1 個**。sidecar は非特権 UID・egress デフォルト遮断・
  ストレージ/DB/OpenFGA クレデンシャルを持たない。兄弟サンドボックス・ホストへは波及しない。
- **orchestrator 侵害の想定被害 = 全サンドボックスの制御**。ただし orchestrator も MinIO/Postgres/OpenFGA の
  クレデンシャルを持たない構成（成果物保存は shiki-server 側で回収後に実施）。ストレージ実体へは到達不能。
- 分離は結合テストで担保（別 PID・一方 kill で他方継続・SandboxId↔transport 1:1・kill 後残留ゼロ）。
- 将来の defense-in-depth（wasm プロセスを gVisor で二重に包む）はポストアルファの検討事項。

## 隔離クラスと NFR-1 の正直な条件（PIT-24）

3 ティア（wasm / gVisor / Firecracker）は隔離強度が異なる。`SandboxBackend::isolation_class()` が
これを型で表明する:

- **Firecracker = `VmLevel`**: KVM microVM。**NFR-1「サンドボックスは VM 級隔離」を満たすのは KVM が
  ある環境に限る**（`/dev/kvm` 必須）。KVM 非搭載環境では Firecracker ティアは利用不可。
- **gVisor = `UserspaceKernel`**: ユーザ空間カーネル。KVM 無しで動くフォールバックだが VM 級より一段弱く、
  脱出 CVE 実績がある。**NFR-1 を厳密には満たさない**。加えて cgroups が使えないデプロイでは
  メモリ上限がソフト（`--total-memory`＋orchestrator watchdog による二重防御）。ハードなメモリ/VM 級隔離が
  要件なら Firecracker ティアを使う。
- **wasm = `WasmProcess`**: V8＋wasm の仮想 FS/net。**既定ティアではない**（2026-07 に既定は gVisor へ移した・
  design §4.6）。web_fetch のような Python を伴わない短命実行で使う。

`validate::check_isolation()` は将来の機微度モデル導入時に、機微ワークロードで `UserspaceKernel` 要求を
拒否/警告するポリシフック（現状は allow-all・隔離クラスは create 監査へ記録）。**NFR-1 は「KVM 前提」と
条件付きで読むこと**。egress のホスト名 allowlist は gVisor/FC ティアでは SNI プロキシ（非特権 user+net
namespace＋偽 DNS）で実効化する（PIT-25）。

## エアギャップ配布（PIT-33）

- Pyodide 一式・rusty_v8 アーカイブは実行時にレジストリ取得しない。`asset-manifest.sha256` に pin し、
  `scripts/fetch-sandbox-assets.sh` がビルド前段で検証付き取得。エアギャップは `SANDBOX_ASSET_BASE` で
  同一 SHA のローカルミラーを差す。wasm コマンドスイートは `registry/native` から自前ビルドし製品に同梱。
