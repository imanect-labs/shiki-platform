# ファイル版保存方式の比較

関連: [#488](https://github.com/imanect-labs/shiki-platform/issues/488)。Phase 1 のストレージ基盤と Phase 11 の共同編集に関係する調査。初回比較後の製品実装は [版の物理保存表現](../../docs/storage/version-storage.md) を参照。以下のPython比較は製品のnative Rust実装とは別の測定。

結論と実装提案は [REPORT.md](REPORT.md)、全方式の数値は [主測定の表](results/main/tables.md)。

## 再実行

Python 3.12 と xdelta3 3.0.11 を使用する。依存はプロジェクト専用の venv に入れる。

```bash
python3 -m venv eval/version-storage/.venv
eval/version-storage/.venv/bin/pip install -r eval/version-storage/requirements.txt
XDELTA3=/absolute/path/to/xdelta3 eval/version-storage/.venv/bin/python \
  -m unittest discover -s eval/version-storage -p 'test_*.py' -v
eval/version-storage/.venv/bin/python eval/version-storage/bench.py \
  --output eval/version-storage/results/main --trials 3 --xdelta /absolute/path/to/xdelta3
eval/version-storage/.venv/bin/python eval/version-storage/summarize.py \
  eval/version-storage/results/main
```

`--quick` は小規模動作確認、`--filter` はケース名の部分一致、`--methods` は方式の選択（集計に必要な `full-cas` は自動追加）、`--resume` は同じ出力先での中断再開。再開時は生成データのハッシュに加えてbackend・辞書window・依存版・測定コードのハッシュ等の条件を照合し、一致した完了済み測定だけを再利用する。条件が違う出力や条件記録がない旧出力は、環境情報を書き換えずに拒否する。中断中の未完了測定が残したサンプルは集計時に最後の成功分を採用する。`--select-cases cases.json` はケース名配列による絞り込み。`--backend files` は一時ディレクトリ内に各実体を別ファイルとして保存する追加測定で、OS ページキャッシュ有効・fsync なし。

この実測で使う xdelta3 は Debian の `xdelta3_3.0.11-dfsg-1.2_amd64.deb` をユーザー所有の一時ディレクトリに展開したもの。システムへのインストールは不要。`environment.json` に実行バージョンと環境を残す。

## ケース設計

基本マトリクスは **491 履歴・10,091 版・12 形式・11 方式・各 3 回**。seed は 73 と 941。実利用頻度を仮定した重み付けはしない。

| ケース群 | 履歴数 | 版数/履歴 | 内容 |
|---|---:|---:|---|
| テキスト | 270 | 17 | Markdown・コード・CSV・JSONL・HTML × 約16 KiB/256 KiB/1 MiB × 9変更 × 2 seeds |
| メディア・非圧縮対照 | 64 | 17 | PNG・JPEG・PDF・乱数バイナリ × 2規模 × メタデータ/局所変更/全面変更/取り消し × 2 seeds |
| OOXML 制御編集 | 84 | 17 | docx/xlsx/pptx × 2規模 × 同一/局所/挿入/part追加/ZIP再梱包/大量変更/取り消し × 2 seeds |
| OOXML ライブラリ再保存 | 36 | 17 | 3形式 × 2規模 × 局所編集/追加削除/全体編集 × 2 seeds |
| 既存 fixture | 15 | 17 | リポジトリ内の5ファイル × 局所/ZIP再梱包/取り消し |
| 長期履歴 | 16 | 129 | Markdown/CSV/乱数/docx × 局所/先頭挿入 × 2 seeds |
| 大きいファイル | 6 | 9 | 約4 MiB のコード/乱数 × 局所/末尾追加/全面変更 |

データの境界:

- 顧客の文書や人間の実作業ログは使用していない。既存 fixture も実作業の履歴ではなく、元ファイルから生成した編集履歴。
- OOXML ライブラリ再保存は python-docx/openpyxl/python-pptx の実際の load/edit/save。Collabora・Microsoft Office の保存トレースではない。pptx の `add_delete` はスライド追加のみで、削除を含まない。
- OOXML 制御編集の `head` は XML コメント挿入、`structure` は未参照 ZIP part 追加。見た目の段落挿入やスライド追加の代用にはしない。その区別は `source` とケース群別の集計で維持する。
- テキストの局所変更・削除などはバイト列の制御編集で、UTF-8 や構文の妥当性は保証しない。画像は高エントロピーの生成画像を使用し、自然写真の分布を代表しない。PDF は ReportLab で生成した有効な文書。
- 生成 OOXML の ZIP 時刻と core properties の時刻を固定し、時計による差を除去する。既存 fixture の初版だけはチェックインされた元バイトを保持する。
- サイズ表記は入力生成時の目標。ZIP、画像、PDF の実バイト数は形式ごとに異なり、`corpus.jsonl` に全版の実サイズ・SHA-256 を残す。
- 各履歴は別の実体共有領域で測る。履歴内の同一内容は方式を問わず共有する。異なるファイル間の共有利益や、テナントを跨ぐ共有は計上しない。

## 比較方式

| ID | 保存形式 |
|---|---|
| full-cas | 現行相当。ファイル全体の SHA-256 による同一内容の共有、無圧縮 |
| full-zstd | 全文を独立に Zstd level 3 で圧縮 |
| fixed64-zstd | 固定64 KiBに分割し、同一断片を共有・個別圧縮 |
| cdc16-zstd / cdc64-zstd | FastCDC。平均16/64 KiB、最小平均の1/4、最大平均の4倍。断片共有・個別圧縮 |
| zdict8 / zdict32 | 直前版の**元バイト列を生辞書**として Zstd 圧縮。8/32版ごとに独立全文。汎用バイナリ差分とは別方式 |
| xdelta8 / xdelta32 | 直前版に対する VCDIFF 差分 + Zstd。8/32版ごとに独立全文 |
| zanchor8 / zanchor32 | 独立した基準版を生辞書として各版を直接 Zstd 圧縮。基準版の更新間隔は8/32版。復元は最大2実体 |

Zstd は checksum 付き。圧縮が元データより大きければ無圧縮を使い、差分表現が独立全文より大きければ全文に戻す。差分方式は同一内容の再登場によって予定のチェックポイントが飛んでも復元段数の上限を超えない。基準版方式は独立全文が作られた際に基準を更新する。

xdelta3 は `-3 -D -R -A -S none -B 1048576 -W 1048576`。外部解凍・再圧縮・アプリケーションヘッダ・内部二次圧縮を無効にし、差分自体を共通の Zstd で圧縮する。source/input window は各1 MiB。これは今回測った設定で、xdelta の最適設定を網羅した比較ではない。

## 測定と集計

- 各ケース内で方式の実行順を seed 付きで入れ替え、3回測る。並列測定はせず、入力生成は計測区間の外。
- 保存時間はハッシュ・分割・圧縮・実体辞書への登録を含む。復元時間は実体取得と展開を含む。**元バイトとの比較は計測区間外で、すべての版・方式・反復について行う**。
- 各版の復元はランダム順。復元済み基準版のアプリケーションキャッシュはリクエスト間で使わない。メモリ上の実体辞書と OS ページキャッシュは有効なので、ストレージのコールド読み出しではない。
- xdelta のみ CLI 起動と一時 source ファイル I/O を含む。他方式は Python から native ライブラリを呼ぶ。したがって xdelta の時間差を、native API を使う製品実装でのアルゴリズム差と断定しない。
- `payload_bytes` は実際に生成したバイト列の合計。断片 manifest は断片参照ごと32バイト、差分は基準 SHA-256 32バイトとタグを含む。
- `modeled_bytes` は上記に **実体1件64バイト + 論理版1件48バイト**の共通モデルを加えた値。PostgreSQL の実行・行サイズ・インデックス・WAL・オブジェクトストアの管理領域は測っていない。0/64/256/1024バイトに変えた実体単位の感度分析も出す。
- 最新版の署名付き URL を維持するための全文併置は、最新版の元バイト + 実体管理64バイトを加算した**容量計算**。実際の非同期生成・URL配信は実装していない。
- 形式内の容量比は合計バイトの比率。全体の合計はこのコーパス構成に依存する。形式別・ケース群別・履歴別の中央値も CSV に出し、実利用での削減率とは区別する。
- p50/p95 は全版・全反復のサンプルを集めた分位点であり、各履歴のp95を平均した値ではない。テキスト・OOXML・全面変更・長期履歴などの区分も別に集計する。
- 読出数はこの非pack実装で復元する際の実体アクセス回数。断片の並列取得・先読み・共有キャッシュ・packファイルによる改善は未計測。

`runs.jsonl` にケース×方式×反復の集計値、`samples.jsonl.gz` に `[case, method, trial, version_index, encode_ms, restore_ms, read_count, read_bytes]`、`summary.csv` / `sensitivity.csv` / `tables.md` に再生成可能な集計を残す。

図は `python eval/version-storage/plot.py eval/version-storage/results/main` で再生成する。3反復ごとの時間は `trials.csv`。

追加測定の再実行:

```bash
eval/version-storage/.venv/bin/python eval/version-storage/bench.py \
  --output eval/version-storage/results/files --trials 3 --backend files \
  --select-cases eval/version-storage/results/files/selected-cases.json \
  --methods full-cas full-zstd cdc16-zstd zdict8 zanchor8 zanchor32 xdelta8 \
  --xdelta /absolute/path/to/xdelta3
for winlog in 22 24; do
  eval/version-storage/.venv/bin/python eval/version-storage/bench.py \
    --output "eval/version-storage/results/window-${winlog}" --trials 3 \
    --select-cases eval/version-storage/results/window-cases.json \
    --methods full-cas zanchor32 --dictionary-window-log "$winlog" \
    --xdelta /absolute/path/to/xdelta3
done
```

`--dictionary-window-log` は辞書圧縮の参照 window のみを変える。主測定は既定設定の0で、追加測定は22（4 MiB）と24（16 MiB）。どちらも20履歴・292版・2方式・3反復。`files` は52履歴・1,284版・7方式・3反復。選択後のコーパスSHA-256は主測定と一致することを確認する。

メモリ測定・ローカルファイル追加測定ともに、GCS/RustFS/S3 のネットワーク RTT、HTTP Range、並列取得、DB トランザクション、耐久化、課金、GC、バックアップ、障害復旧は評価対象外。これらの性能を実測済みとは扱わない。

## RustFSでの配置比較

`rustfs_layouts.py` で小オブジェクト、pack全体GET、packのRange GETを比較した。64 KiB/1 MiB/4 MiB × 2 seeds × 局所変更/先頭挿入/散在変更/全面変更/32版の変更蓄積 = **30ケース、3方式、各3回、270回の完全一致復元**。既知の履歴から作った断片を最大1 MiBのpackに詰め、復元時は最大8 GETを並列化する。packは製品形式ではない。

| 配置 | 復元時間中央値 | SDK GET数中央値 | GET数最大 | 必要な符号化バイトに対する転送倍率最大 |
|---|---:|---:|---:|---:|
| 小オブジェクト | 112.50 ms | 62.5 | 271 | 1.00× |
| pack全体GET | 7.94 ms | 2 | 10 | 3.30× |
| pack Range GET | 8.35 ms | 2 | 16 | 1.00× |

全行は [rustfs-layouts.jsonl](results/rustfs-layouts.jsonl)、環境と集計は [rustfs-layouts.environment.json](results/rustfs-layouts.environment.json)。Rangeは同じpack内の隣接した必要範囲をまとめる。GET数はSDK呼出数で、SDK内部の再試行を別途計数していない。

RustFS 1.0.1を同じ開発VMのDockerで使用。Python SDK・warm cache・共有CPUの値で、入力生成・符号化・PUT・検証・削除は時間から除く。native RustのCPU/RSS、実運用RTT、完全なコールド復元、GCやバックアップの性能値には使用しない。pack/RangeでHTTPリクエストを減らせることと、pack全体取得の転送増を確認する測定。製品は現在のObjectStore境界を保ち、小オブジェクト＋最大8並列GETと、方式選択時のGETコスト設定を実装する。

再実行は専用のローカルRustFSを起動し、`STORAGE_TEST_S3_ENDPOINT`、`STORAGE_TEST_S3_ACCESS_KEY`、`STORAGE_TEST_S3_SECRET_KEY` を設定して行う（資格情報は出力しない）。専用bucketの一意prefixを使い、測定終了時に作成した実体だけを削除する。

```bash
eval/version-storage/.venv/bin/python eval/version-storage/rustfs_layouts.py \
  --output /tmp/version-storage-rustfs-layouts.jsonl
```

## SQL実行計画

`sql_plans.sql` は専用テストDBに10万blob/10万版/1万ジョブの合成メタデータを作り、最後にROLLBACKする。オブジェクトのバイトは書き込まない。[実行計画](results/sql-plans.txt)を保存した。RLSを有効にしたruntimeロールで、重複ジョブの点検索は `job_queue_version_storage_pending_idx` を使用して0.137 ms、履歴末尾の16版取得は版番号のindexとblobの主キーを使用して0.229 ms。64 MiB超の10万blobを置いたbackfillはサイズ先頭のindexを使い、0.818 msだった。

2026-11-15のローカルPostgres・warm cache・共有開発VMでの実測。符号化・ネットワークGET・復元を含む性能値ではない。時間を本番のSLOとせず、走査範囲と索引の利用を確認する。RLS用の索引はJSONB式を直接索引にせず、トリガーで同期する通常列のtextキーで絞り、実際のJSON内容も残余条件で照合する。

```bash
psql -v ON_ERROR_STOP=1 -f eval/version-storage/sql_plans.sql "$STORAGE_TEST_DATABASE_URL"
```

GC予定の走査は、[専用SQL](gc_scopes_plan.sql)で10,000組織すべてにbackup holdを設定して測定した。[実行計画](results/gc-scopes-plan.txt)は0.764 msで、予定索引から16組織だけを固定し、hold台帳の主キーで保護期限を調べる。blob件数に比例した再走査は行わない。`shiki_maintenance`ロールで実行し、合成データはROLLBACKする。
