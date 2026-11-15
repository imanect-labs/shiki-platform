# 版の物理保存表現

[#1297](https://github.com/imanect-labs/shiki-platform/issues/1297)（関連 [#488](https://github.com/imanect-labs/shiki-platform/issues/488)） / [比較実測と提案](../../eval/version-storage/REPORT.md)

## 論理版と実体

`node_version → blob → storage_manifest → blob_storage_object` の順で解決する。版番号・作者・元のサイズ・内容SHA-256は変えない。`blob.refcount` は従来どおり論理版の参照数。物理実体の参照は別の `blob_storage_ref` と、そのINSERT/DELETEトリガーによる `blob_storage_object.refcount` で管理する。

manifestの形式番号は1。物理キーは `{tenant_id}/{org}/storage-v1/{encoded_sha256}`。圧縮済みバイトにもSHA-256を持ち、展開後は元サイズと元SHA-256を照合する。共有範囲はtenant/org内に限る（PIT-14）。

| 表現 | 内容 | 復元 |
|---|---|---|
| Full | 独立した全文。Zstd level 3＋checksum。圧縮で増える場合はraw | 1実体 |
| Dictionary | 独立Fullを生辞書として圧縮した本文 | 最大2実体。差分連鎖は作らない |
| Chunks | FastCDC v2020。最小4/平均16/最大64 KiBの断片列を個別圧縮・共有 | 重複実体は1回取得。最大8 GETを並列化 |

圧縮/分割はファイル形式や人間/AIの区別を持たない。候補を実際に符号化し、既存実体との共有、manifestのサイズ、実体管理64 bytes、設定した読出コストで選ぶ。最初の断片化では**既に保存されている次版**と共通の断片の費用を2版に按分する。将来の編集を予測して共有率を仮定しない。基準版は過去31版の範囲から独立Fullを探す。

`storage_bytes` はその表現が新たに持つ符号化バイト数、`storage_reads` は復元に必要な一意の物理実体数。`storage_bytes` の合計は共有や基準実体を考慮した物理使用量そのものではない。物理台帳とrawキャッシュを合わせて観測する。

## 非同期最適化

有効時は版の記録と同じトランザクションで既存jobqの `storage_version_optimize` へ投入する。payloadはorg/node_id/after_versionで、未着手ジョブはorg/node_idごとにまとめ、最も古いcursorを残す。通常保存は直前の通常版から始め、提案版を起点にしない。処理は1回最大16の論理版。版番号cursorをジョブに持ち、長い処理済み接頭部を再走査しない。古い版から処理し、現在いずれかのnodeが参照する内容と提案だけの内容は最適化対象から外す。同じ内容を他の旧版が参照している場合は表現を共有する。

workerは既存履歴も64 blobずつbackfillする。ジョブ投入と同じトランザクションで確認済みにし、部分indexから外す。現在版や提案だけのblobも確認済みにする。worker停止中も内容更新は耐久キューへ記録するため、後に旧版になった内容も再開後に処理できる。再試行・DLQ・テナント間の公平性はjobqを使用。1プロセス内のworkerは直列処理。保守APIを並行実行しても符号化は最大2、実体の読み出し・展開は最大4を上限にして、圧縮はblocking poolに逃がす。読取permitの保持中はDB接続・行ロックを取得せず、DBとsemaphoreの循環待機を避ける。raw再生成のPUTや親トランザクションが保持するバイトも含め、総メモリには接続数ぶんの余裕を取る。

登録・公開の順序:

1. 物理実体を台帳に登録し、公開前の保護期間をcommitする。
2. 実体を書き込む。途中で落ちても台帳が回収対象を保持する。
3. 論理blobと物理実体の行を固定し、ストアから読み戻して内容のSHA-256を検証する。
4. 同じDBトランザクションで物理参照とmanifestを公開する。GCはこの間、該当実体を削除できない。

再配信や二重処理でも論理版を追加しない。既に公開した表現はそのまま使う。64 MiBを超える内容はrawのまま保持する。`max_optimize_bytes` は1バイト〜64 MiBの入力上限で、作業メモリは基準・次版・符号化候補を含めてその数倍になる。より大きなファイルの分割には現在の一括ObjectStore APIをストリーム化する別の設計が必要。

## rawキャッシュとGC

最新版は配信可能なrawを保持する。旧版の署名付きURLが必要なら、元内容を再生成して同じrawキーへ置く。URLの署名とleaseの更新を行ロック内で実施し、署名後にDB時計からTTL＋60秒のleaseを記録してからURLを返す。内部索引用URLにも同じ処理を使う。

導入前に発行されたS3署名付きURLの最大寿命を保護するため、migrationでは既存rawを7日間保持する。導入後の旧版は、最適化の公開から `raw_grace_secs` と発行済みURLのleaseの長い方まで保持する。raw回収後の監査予定 `raw_audit_at` は、再生成したダウンロードキャッシュのleaseとは別に管理する。

GCは次を保護する。

- nodeが現在参照するraw。ゴミ箱内の現在版も含む。
- 有効なURL/export leaseのあるraw。
- manifestから直接・間接に参照される物理実体。
- 公開前に台帳へ登録した実体の保護期間（1日）。
- バックアップholdのあるtenant/orgのrawと物理実体。

現在の論理版・提案版・履歴はGCで削除しない。物理参照がゼロの実体と、期限を過ぎたrawキャッシュを回収する。回収した物理実体の台帳はtombstoneとして残す。タイムアウト/中断後に遅延PUTが完了した場合も、次の監査で回収できる。

rawは1回に有効フラグのある候補64件、欠落フラグの監査16件、物理実体は16件まで。期限走査には `now()` と部分indexを使い、参照付き全実体のanti-joinを繰り返さない。削除はraw/物理実体ごとの `delete_batch` と一括SQLで行い、1回あたりストア削除要求を最大2回に抑える。欠落フラグも監査するため、全文再生成後の親トランザクションrollbackによるrawを取り残さない。

## 設定と導入

readerは常に全表現を解決する。worker/GCは既定で有効で、設定を省略しても旧版を非同期に最適化する。`SHIKI_CONFIG` で指定したTOML、または既存の `SHIKI__` 環境変数で設定する。

有効時は `database.max_connections` を3以上にする（既定10）。保守ロックと処理用接続を確保するため、3未満は起動時に拒否する。内部読取同士は共有行ロックを使い、GC・表現公開と排他する。

旧readerが混在するローリング更新では、更新開始前に `enabled=false`（環境変数なら `SHIKI__STORAGE__VERSION_STORAGE__ENABLED=false`）を全サーバーに設定する。すべてのreaderを更新した後でこの上書きを外す。一括停止・一括更新や新規導入では既定値のまま使える。migrationは既存rawを7日間保護するが、旧readerとの無期限の混在を保証するものではない。

```toml
[storage.version_storage]
enabled = true
raw_grace_secs = 86400
max_optimize_bytes = 67108864
read_cost_bytes = 0
```

`read_cost_bytes=0` は容量優先。1実体の読出しをその設定値分の保存bytesとして評価し、値を大きくするとGET数の少ない表現を選びやすくなる。設定範囲は0〜1 MiB。既に公開した表現を設定変更だけで再圧縮はしない。値は実ストアのRTT、GET課金、旧版のアクセス頻度を見て決める。例えば8192では1 GETを8 KiBとして評価する。

[RustFSでの追加比較](../../eval/version-storage/README.md#rustfsでの配置比較)ではpack/RangeによるGET削減を確認した。現ObjectStoreにはRangeがなく、pack全体の取得には最大3.30倍の転送増があった。本実装は既存のObjectStore境界で使える小オブジェクト＋最大8並列GETを採用する。多数の断片が必要な内容は容量とGET数を設定で調整できる。pack/Rangeの製品導入、native Rustのコールド復元CPU/RSS・本番RTTの性能評価は今後の対象。

明示的に無効にする場合の環境変数は `SHIKI__STORAGE__VERSION_STORAGE__ENABLED=false`。Composeではoverrideの `shiki-server.environment` などから渡す。無効化すると新たな最適化・GCを止める。保存済み表現を読むため、新readerは引き続き必要。

旧readerへ切り戻す場合はworkerを無効化し、保守用 `BlobStorage::materialize_for_export` で必要な全blobをrawへ戻してから切り替える。通常の無効化だけで圧縮済み実体がrawへ戻るわけではない。

## バックアップとテナント撤去

バックアップ処理は、DBとオブジェクトの整合点を取る**前**に `BlobStorage::hold_backup(tenant, org, id, ttl)` をcommitする。orgが複数ある場合は対象orgすべてに取得する（PIT-38）。同じidを使って期限前に延長できる。処理中はrawと物理実体のGCを止め、終了時に `release_backup` で解除する。

leaseを失ったバックアップは成功扱いにせず、新しい整合点から取り直す。DBだけのPITRと現在のオブジェクト群を無条件に組み合わせず、manifest・物理参照・実体を含むバックアップを取得する。raw形式のexportには `materialize_for_export` を使える。

テナント撤去は保守書込と排他する。バックアップhold中は撤去を拒否する。deleting/deletedのテナントへの新たな最適化・全文再生成・バックアップ開始も拒否する。新テーブルは既存のテナント撤去の導出対象となる。

`shiki-admin retenant` はデータプレーンを停止した保守時間中に実行する。旧/新tenantの保守ロックを旧キー削除まで保持し、バックアップhold中は外部副作用より前に拒否する。manifestが必要とする実体をコピー・ハッシュ検証し、DBのtenant_id変更をcommitした後に旧キーを削除する。圧縮実体の台帳と参照も移行する。コピーや削除の再実行は冪等。

## 検証

`crates/storage/tests/version_storage_it.rs` が実Postgres/OpenFGA/RustFSを使い、旧版URL・内部索引URL・復元・提案の採用、129版の全版復元、2 MiBバイナリの断片共有、並行最適化・GC、バックアップ保護、破損時の停止、テナント越境拒否、中断後の孤児回収、既存履歴backfill、旧DBのmigration、圧縮済み履歴のtenant移行を検証する。

```bash
SHIKI_REQUIRE_IT=1 cargo test -p shiki-storage --all-features -- --test-threads=1
```

接続先は既存と同じ `STORAGE_TEST_DATABASE_URL` / `OPENFGA_TEST_URL` / `STORAGE_TEST_S3_*`。ローカル・CIともS3互換ストアはRustFSを使う。

2026-11-15のローカル検証: storage全184件が成功し、最終変更後の14件の結合テスト（追加1件を含む）も成功。API設定56件、fmt、storage/APIのall-targets・all-features clippy、tenant SQL、ファイル行数、migration番号、CI対応表、依存検査が通過。変更行カバレッジは1311/1345行（97.47%）、未計測の変更元クレートなし。DB/permitの逆順、撤去後の再投入、共有読取、cursor統合、提案版が挟まる履歴も検証した。

[SQL実行計画](../../eval/version-storage/README.md#sql実行計画)では10万版・1万ジョブを用いて索引の利用と有界走査を確認した。pack/Rangeの270回の測定はPython SDKによるもので、Rust機能テストやSQLの測定と区別する。
