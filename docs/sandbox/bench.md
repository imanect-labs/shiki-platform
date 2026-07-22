# サンドボックス 3 ティア ベンチマーク

`crates/sandbox-bench`（gated バイナリ）が各ティアを **Backend トレイトで in-process 構築**し、
フルライフサイクル（create→exec→python→IO→destroy）を反復計測する（gRPC は挟まない＝純バックエンドコスト）。
criterion は使わない（実ランタイム横断のライフサイクル計測のため）。

## 実行方法

```bash
# wasm（sidecar）＋gVisor を計測。Firecracker は /dev/kvm がある KVM ホストで FC_* を足す。
SANDBOX_BENCH=1 BENCH_ITERS=6 \
  SECURE_EXEC_SIDECAR_BIN=vendor/secure-exec/target/release/secure-exec-sidecar \
  RUNSC_BIN=/usr/local/bin/runsc GVISOR_ROOTFS=deploy/sandbox-assets/rootfs \
  NETNS_HOLDER_BIN=target/debug/shiki-netns-holder \
  FC_BIN=deploy/sandbox-assets/bin/firecracker FC_KERNEL=deploy/sandbox-assets/vmlinux.bin \
  FC_ROOTFS=deploy/sandbox-assets/rootfs.ext4 \
  cargo run -p shiki-sandbox-bench
```

各ティアはランタイム/アセットが env で揃うときのみ計測する（欠けたら「未計測」に理由付きで載る）。
出力は markdown（stdout）＋JSON（`target/bench/results.json`）。

## シナリオ

- **create→ready**: `create()` が使用可能なインスタンスを返すまで（wasm=VM 準備、gVisor=`runsc run`＋running 待ち、
  FC=boot＋vsock Ready）。
- **exec**: 自明な Python `print(1)` の 1 往復（shell コマンド suite に依存せず全ティア比較可能）。
- **python**: 純 Python CPU（`sum(i*i for i in range(300000))`・numpy 非依存）。
- **put/get 1MiB**: 1 MiB のファイル書き→読みの往復（wasm=GuestFilesystemCall、gVisor=host bind、FC=agent）。
- **destroy**: 破棄完了まで。
- **RSS**: create 直後のバックエンド子孫プロセス（sidecar/runsc）の VmRSS 合計（cgroup 無し環境の近似）。

## 計測結果（2026-07-06・開発ホスト）

環境: 非特権 LXC（Proxmox・KVM 非搭載）・rootless。wasm=secure-exec（V8＋Pyodide）、
gVisor=runsc systrap rootless＋`python:3.12-slim` rootfs。**Firecracker は `/dev/kvm` が無いため未計測**
（実装・アセットは用意済み・API 層は実 firecracker で検証済み・boot は KVM ホストで `SANDBOX_FC_IT=1`）。

| ティア | N | create→ready p50 / p95 (ms) | exec p50 (ms) | python p50 (ms) | put/get 1MiB p50 (ms) | destroy p50 (ms) | RSS p50 (MB) |
|---|---|---|---|---|---|---|---|
| wasm | 6 | 11.5 / 13.1 | 6469 | 6019 | 19.6 | 14.9 | 21 |
| gvisor | 6 | 132.3 / 134.4 | 59.6 | 81.8 | 0.8 | 30.5 | 104 |
| firecracker | — | 未計測 | — | — | — | — | — |

## 解釈（ティア選択の指針）

- **wasm は起動が桁違いに軽い（~12ms・21MB）**が、**Python 実行は非常に重い（~6s）**: secure-exec は exec ごとに
  Pyodide/CPython-on-WASM を初期化するため、計算そのものより初期化コストが支配的。
  **Python を回さない短命実行（web_fetch 等）向き**。
- **gVisor は起動が重め（~130ms・104MB）だが native CPython が ~75× 速い（82ms vs 6019ms）**。
  ファイル I/O も host bind で高速（0.8ms）。**重い Python・任意 pip・ネイティブツールを回す用途**はこのティア。
  KVM 不要で動く（PIT-24: ユーザ空間カーネルゆえ VM 級より隔離は一段弱い）。
- **Firecracker は VM 級隔離（NFR-1）**。KVM 前提。契約上 VM 級が要る機密ワークロード向け。起動は FC の
  実測（~125–250ms級のboot＋agent Ready）を KVM ホストで別途取得する。

要点: **Python を回さない軽量短命＝wasm、Python/native 実行＝gVisor、最強隔離＝Firecracker** という住み分け。

> **この表の当初の結論（「既定は wasm のままで正しい」）は撤回した（2026-07）。** create レイテンシだけを
> 見て wasm を選んでいたが、code_interpreter の実効体感は **create + 実行の総時間**で決まる。
> 同じ Python 実行で **wasm ≒ 11 + 6019 ms に対し gVisor ≒ 132 + 82 ms** であり、gVisor が一桁以上速い。
> よって **code_interpreter の既定ティアは gVisor**（design §4.6）。wasm は **web_fetch**（egress を単一ホストへ
> 固定する短命・読み取り専用実行。egress allowlist を wasm の仮想 net ホスト関数で実効化）に残す。
> ⚠️ web_fetch は内部で urllib（Python）を実行するため wasm でも exec ごとに Pyodide 初期化コストを払う点は既知の課題
> （wasm を選ぶ理由は速度ではなく egress モデル）。
> ⚠️ 既定切替には native rootfs への numpy/pandas 同梱が前提（下記「注意」参照）。

## 注意

- 数値は 1 ホストの相対比較であり絶対性能保証ではない。RSS は子孫 VmRSS の近似（cgroup 無し）。
- wasm の Python は Pyodide 同梱 numpy/pandas を持つ（gVisor の slim rootfs は numpy 非同梱）。本ベンチは
  公平性のため numpy 非依存の純 Python を使う。
- Firecracker 行は KVM ホストでの再計測で埋める（本表は開発ホストの制約を正直に反映）。
