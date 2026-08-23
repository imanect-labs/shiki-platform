# OpenJTD フォーク運用ポリシ

> 一太郎（JTD）対応の基盤である [OpenJTD](https://github.com/KimEJ/OpenJTD) を
> **我々が所有するフォーク**として保守する方針。`vendor/secure-exec` と同じ扱いで、
> [docs/sandbox/fork-policy.md](../sandbox/fork-policy.md) と対になる文書。

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
- **shiki 本体が依存するのは `rjtd-core` だけ**。
  同梱の `rjtd-export`（pdf/svg 出力）・`rjtd-cli`（解読プローブ）・`rjtd-wasm`（WASM ビューア）には
  依存しない。`Cargo.lock` に増えるのは `cfb` 1 つで、供給元の面積はほとんど広がらない。
- `rjtd-cli` を捨てずに残しているのは、そこにある `table-candidates` / `page-marks` /
  `text-control-ranges` といったプローブ群が、我々のレイアウト解読の測定器そのものだから。
  依存グラフには載らないので、抱えるコストは容量だけ。

  ```bash
  cd vendor/openjtd/rjtd && cargo run -p rjtd-cli -- table-candidates <file.jtd>
  ```

  **ただし `vendor/openjtd/rjtd/Cargo.lock` の依存（`image` / `resvg` / `usvg` / `tiny-skia` /
  `wasm-bindgen` 等 100 crate 超）は `cargo deny` の走査対象外**である。shiki の依存グラフに
  入らないので当然だが、裏を返すと **RUSTSEC の監視外**ということでもある。
  `rjtd-cli` は開発者ローカルの調査専用とし、**素性の分からない `.jtd` を食わせない**。

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

### 現在のパッチ

- **`0001-bound-difat-walk.patch`** — lenient CFB リーダの DIFAT 走査を入力サイズで有界化する。
  ヘッダの `fat_sector_count` / `difat_sector_count` は攻撃者制御の u32 で、自己参照する DIFAT
  セクタと組み合わせると **1 KiB のファイルで `sector_ids` が 2 GiB を超え、確保失敗でプロセスが
  abort** した。abort は unwind ではないので `catch_unwind` では捕まえられず、API 全体が落ちる。
  併せて `sector_size < 4` での `sector_size / 4 - 1` の underflow も潰した（debug ではパニック、
  release では `usize::MAX` 回のループ）。兄弟の走査（`collect_sector_ids` / `read_sector_chain`）は
  既に visited セットを持っており、ここだけが漏れていた。**同じ穴は上流にもあるので PR 化する。**
  回帰テストは `crates/jtd/tests/adversarial_it.rs`。
- **`0002-iterative-directory-walk.patch`** — ディレクトリツリーの走査を再帰から明示スタックの
  反復へ置き換える。`assign_child_tree_paths` は visited セットは持つが**深さ上限が無く**、
  しかも何より先に `left_id` へ再帰するため、再帰深度がエントリ数（＝入力サイズでしか
  縛られない）と等しくなっていた。**約 1 MiB の細工ファイルでスタックオーバーフロー → abort**。
  兄弟リンク（`left`/`right`）はパスが伸びないので id だけを反復し、`child_id` の下降にだけ
  深さ上限（16）を掛ける。**深さだけでは足りない**ので、パス文字列の総バイト数も有界にした
  （8 MiB の入力に約 65,000 エントリを置き、深い prefix の下に並べると同じ prefix が
  数百 MiB ぶん複製される）。**同じ穴は上流にもあるので PR 化する。**
- **`0004-utf16-surrogate-pairs.patch`** — UTF-16 のサロゲート対を結合してデコードする。
  `is_invalid_scalar` が各 code unit を単独で無効と見なすため、**U+10000 以上の文字が本文から
  無言で落ちていた**。日本語文書では他人事ではなく、氏名に CJK 拡張 B（`𠮷`・`𩸽`）が普通に
  使われるので「𠮷田」が「田」になっていた。**同じ穴は上流にもあるので PR 化する。**
- **`0003-linear-embedded-text-dedup.patch`** — 埋め込みテキスト断片の重複排除を線形走査から
  `HashSet` へ置き換える。`SsmgV.01` は入力のどこにあってもよく 18 バイトで 1 断片になるため、
  断片ごとに既出テキスト全体と比較する実装は **O(N²)** だった（実測 1 MiB で 29 秒、
  2 MiB で 133 秒。しかも `detect_format` と本文読みの両方から呼ばれて 2 回走っていた）。
  **同じ穴は上流にもあるので PR 化する。**

`patches/` に記録されていない改変は `scripts/update-openjtd.sh` で**無言で消える**。
vendored ツリーへ直接手を入れたら必ず patches/ にも切り出すこと。
スクリプトは同期の**前に**全パッチをドライランし、当たらなければ何も壊さずに止まる
（上流が同等の修正を取り込むと必ずここで落ちる。それが期待動作）。

## 品質ゲートの扱い

`vendor/` は自作コードの規約を当てない（`docs/sandbox/fork-policy.md` と同じ）。

- **1 ファイル 1000 行**（`scripts/check-file-size.sh`）: 対象外。
  `rjtd-model/src/lib.rs` は **90,000 行超**（次点の `rjtd-cli/src/main.rs` も 10,000 行超）あり、
  これが取り込み可否の前提になっている。
- **カバレッジ 80%**: 対象外（`ci.yml` の `--ignore-filename-regex` に `vendor/(secure-exec|openjtd)/`）。
- **clippy / machete**: workspace 外なので当たらない。
- **`cargo deny`**: **当たる**。依存グラフ全体を走査するため、vendor 経由で入る crate も検査される。
  現時点で入るのは `cfb` のみ。

## 入力は敵対的として扱う

JTD はユーザーがアップロードした外部由来のバイナリで、パーサは Rust なので
**`shiki-server` のプロセス内**で動かせてしまう。資源枯渇もパニックもそのまま API 全体の
可用性に効くため、`crates/jtd` の境界で次を守る。

> **⚠️ どこで走らせるかは未確定（human 判断待ち）。** 既存の文書パースのチョークポイントは
> `DocumentParser` トレイト（`crates/rag/src/parser.rs`）で、既定実装は ingestion-worker への
> HTTP 呼び出しである。JTD を in-process で解くのは**その差し替え点の外に経路を作る決定**で、
> CLAUDE.md の「トレイト境界の変更は human に確認」に当たる。選択肢は
> ①`DocumentParser` の裏に入れる ②worker/別プロセスへ出す ③in-process のまま
> semaphore ＋ `spawn_blocking` ＋ wall-clock timeout を必須にする、の 3 つ。
> **`crates/jtd` を実際に配線する前に決め、結論を `docs/design.md` に書く。**
> 下の防御は、どれを選んでもパーサ自身に必要な最低限である。

- 資源上限を `JtdLimits` で必ず掛ける。既定は上流の `ParseLimits::DEFAULT`（入力 64 MiB・
  展開 256 MiB）より厳しい値（入力 8 MiB・展開 8 MiB・展開率 64 倍・比率下限 64 KiB）。
- **実効的に効くのは入力サイズだけ**だと理解しておくこと。展開上限は上流の契約上
  LH5（`.jtdc`）経路にしか掛からず、本命の非圧縮 `/DocumentText` は上限を受け取らない。
  しかもパーサの中間表現は入力に対して増幅する（実測: `0x001D` を敷き詰めた入力で**約 40 倍**、
  正当な文書でも**約 8.6 倍**）。だから入力上限を実物の分布（60〜100 KB）に照らして
  8 MiB まで締めてある。
- 上流呼び出しは `catch_unwind` で囲み、パニックを 1 リクエストのエラーに閉じ込める。
  `rjtd-core` は `unsafe_code = forbid` なので未定義動作は無いが、細工されたオフセットによる
  範囲外パニックは残りうる。
- **`catch_unwind` を万能だと思わないこと。** 捕まえられるのは unwind するパニックだけで、
  次の 2 つは素通りする。パーサ側で有界にするしかない。
  - **確保失敗による abort** — Rust の OOM は unwind せずプロセスごと落ちる。`0001` で踏んだのがこれ。
  - **スタックオーバーフローによる abort** — 同じく unwind しない。`0002` で踏んだのがこれ。
  - **無限ループ／二次時間** — そもそも戻ってこないので捕捉の機会が無い。`0003` で踏んだのがこれ。
  したがって、細工入力に対しては「エラーを返すこと」ではなく **「有界な時間とメモリで返ること」**を
  テストで固定する（`crates/jtd/tests/adversarial_it.rs` は経過時間もアサートしている）。
- 解析失敗の理由はユーザーへ返さない（フォーマット解析のオラクルにしない）。詳細は `tracing` へ。

## 対象バージョンのスコープ

- **一太郎 8〜13 系（CFB ＋ `/DocumentText`）が対象。** 実サンプル 3 本はすべてこれ。
- 一太郎 2004（v14）以降 / `.jtdc` 圧縮形式は**現時点ではスコープ外**。
  上流は `.jttc` の `-lh5-` 展開を実装済みなので後付けは可能だが、今は広げない。
  `JtdFormat::CompressedDocument` として認識だけはする。
