# OpenJTD フォーク運用ポリシ

> 一太郎（JTD）対応の基盤である [OpenJTD](https://github.com/KimEJ/OpenJTD) を
> **我々が所有するフォーク**として保守する方針。`vendor/secure-exec` と同じ扱い
> （[docs/sandbox/fork-policy.md](../sandbox/fork-policy.md) と対）。

## なぜ自前で持つのか

日本の官公庁・学会は現在も一太郎形式で様式を配布している（厚生労働省の研究計画書、
日本コンクリート工学会の和文原稿テンプレート等）。これを shiki で扱うには JTD を読む必要があるが、

- **Collabora / LibreOffice は JTD を読めない。** Linux 版に一太郎フィルタが無く、
  `soffice --headless --convert-to docx` は Writer 扱いにこそなるものの、実体はテキスト
  フィルタへのフォールバックで、CFB の生バイトをそのまま吐く（完全な文字化け）。
  かつて OpenOffice.org にあった Ichitaro Document Filter は Windows 専用の拡張であり、
  我々のデプロイ先には存在しない。
- したがって **既存の Office 経路（Collabora / WOPI）は流用できず**、変換器を自前で持つしかない。

JTD のリバースエンジニアリングをゼロからやり直す合理性は無い。OpenJTD は CFB コンテナの読解と
`DocumentText` のトークン化まで到達しており（Apache-2.0）、ここに乗るのが最短で、
かつ我々が本当に解くべき問題（表・罫線・ページ幾何 → OOXML）に集中できる。

## 位置づけ

- `vendor/openjtd/` は shiki が**所有するソース**。上流の破壊的変更に追従する義務は負わない。
  pin は `vendor/openjtd/UPSTREAM`（commit SHA）。
- ライセンスは Apache-2.0。`LICENSE` と `THIRD_PARTY.md` を保持する。
- **shiki 本体が依存するのは `rjtd-core` と `rjtd-model` の 2 つだけ**（現時点では `rjtd-core` のみ）。
  同梱の `rjtd-export`（pdf/svg 出力）・`rjtd-cli`（解読プローブ）・`rjtd-wasm`（WASM ビューア）には
  依存しない。`Cargo.lock` に増えるのは `cfb` 1 つで、供給元の面積はほとんど広がらない。
- `rjtd-cli` を捨てずに残しているのは、そこにある `table-candidates` / `page-marks` /
  `text-control-ranges` といったプローブ群が、我々のレイアウト解読の測定器そのものだから。
  依存グラフには載らないので、抱えるコストは容量だけ。

  ```bash
  cd vendor/openjtd/rjtd && cargo run -p rjtd-cli -- table-candidates <file.jtd>
  ```

## 上流がどこまで解いているか（我々の担当分の境界）

`vendor/openjtd/TODO.md` のマイルストーン表が正本。2026-08 時点の要約:

| 層 | 上流の状態 | 我々の担当 |
| --- | --- | --- |
| CFB コンテナ | 実装済み（壊れた FAT の lenient fallback 込み） | そのまま使う |
| `DocumentText` トークン化 | 実装済み。本文・ルビ・`.jttc` の LHA 展開まで | そのまま使う |
| 文書モデル | `Block = Paragraph \| Unknown` の最小形。**`Table` が無い** | 表・罫線を足す |
| ページ幾何 | 未解読。`PageMark` の意味論は `page-mark-u16-geometry-semantics-unproven` と自己申告 | 解読する |
| 出力 | text / md / html / json / pdf。pdf は A4@72dpi 決め打ちの流し込み | **docx を自前で書く** |

RFC は `vendor/openjtd/openjtd-spec/rfc/` にある。解読作業の出発点は
`0003-document-text`（本文ストリーム）・`0006-document-text-position-tables`・
`0007-layout-mark-streams`（`PageMark` / `PaperMark` / `LineMark`）。

## 上流との関係（任意 cherry-pick）

- 上流追従は**周期義務ではなく必要駆動**。欲しい修正が出たときに cherry-pick する。
- ローカル変更は `vendor/openjtd/patches/` に**番号付き最小 diff**で置き、`UPSTREAM` に列挙する。
  意味のある解読成果（表構造・ページ幾何）は上流 PR 化を試みる。ここは我々の競争優位ではなく、
  日本の文書資産を開けるようにすること自体に価値がある。
- 再 vendor は `scripts/update-openjtd.sh`（clone → サブセット抽出 → patches 適用 → ビルド確認）。

## 品質ゲートの扱い

`vendor/` は自作コードの規約を当てない（`docs/sandbox/fork-policy.md` と同じ）。

- **1 ファイル 1000 行**（`scripts/check-file-size.sh`）: 対象外。
  `rjtd-model/src/lib.rs` は 11,000 行超あり、これが取り込み可否の前提になっている。
- **カバレッジ 80%**: 対象外（`ci.yml` の `--ignore-filename-regex` に `vendor/(secure-exec|openjtd)/`）。
- **clippy / machete**: workspace 外なので当たらない。
- **`cargo deny`**: **当たる**。依存グラフ全体を走査するため、vendor 経由で入る crate も検査される。
  現時点で入るのは `cfb` のみ。

## 入力は敵対的として扱う

JTD はユーザーがアップロードした外部由来のバイナリで、しかも docx/pdf と違って
パースが **worker 往復ではなく `shiki-server` のプロセス内**で走る。資源枯渇もパニックも
そのまま API 全体の可用性に効くため、`crates/jtd` の境界で次を守る。

- 資源上限を `JtdLimits` で必ず掛ける。既定は上流の `ParseLimits::DEFAULT`（入力 64 MiB・
  展開 256 MiB）より厳しい値（入力 32 MiB・展開 64 MiB・展開率 64 倍）。
- 上流呼び出しは `catch_unwind` で囲み、パニックを 1 リクエストのエラーに閉じ込める。
  `rjtd-core` は `unsafe_code = forbid` なので未定義動作は無いが、細工されたオフセットによる
  範囲外パニックは残りうる。
- 解析失敗の理由はユーザーへ返さない（フォーマット解析のオラクルにしない）。詳細は `tracing` へ。

## 対象バージョンのスコープ

- **一太郎 8〜13 系（CFB ＋ `/DocumentText`）が対象。** 実サンプル 3 本はすべてこれ。
- 一太郎 2004（v14）以降 / `.jtdc` 圧縮形式は**現時点ではスコープ外**。
  上流は `.jttc` の `-lh5-` 展開を実装済みなので後付けは可能だが、今は広げない。
  `JtdFormat::CompressedDocument` として認識だけはする。
