-- #440: 高チャーンテーブルの autovacuum/fillfactor をテーブル個別に設定する。
--
-- autovacuum の既定発動条件は「生存行数の 20% がゴミになったら」（autovacuum_vacuum_scale_factor
-- = 0.2）。この**比例**条件は「行数は多いが更新は一部行に集中する」形のテーブルで破綻する。
--
-- step_execution がまさにそれで、terminal な step が数百万行積み上がる一方、更新を出しているのは
-- 「今動いている数百件」だけ。それでも発動は数百万行を基準に決まるため、数十万件のゴミが溜まるまで
-- 掃除が始まらない。partial index は述語を外れた行の古いエントリを vacuum まで保持するので、
-- この待ち時間がそのまま claim の劣化になる。
--
-- 実測（Postgres 16・20 万行・ready 500 件・同一スキーマの 2 テーブルへ同じ churn を並行投入。
-- 375 トランザクション × 500 行 = 各 187,500 タプル。autovacuum 有効・naptime は実験時間短縮のため 5 秒）:
--
--   | 設定   | autovacuum | 残 dead | ready partial index | claim（churn 直後） |
--   |--------|-----------:|--------:|--------------------:|--------------------:|
--   | 既定   |       5 回 |  13,500 |              768 KB |  334 バッファ/1.11ms |
--   | 本設定 |      25 回 |       0 |              176 KB |    4 バッファ/0.06ms |
--
-- claim の仕事量で 83 分の 1。なお 2 回目以降のスキャンは既定側も 16 バッファまで落ちる
-- （初回スキャンが死んだ index エントリに LP_DEAD ヒントを付けるため）。運用ではゴミが継続的に
-- 増えるので実際はこの中間に居続けることになる。
--
-- 本番規模ではこの差はさらに開く。既定の閾値は行数に比例するので 200 万行なら 40 万件のゴミを
-- 待つのに対し、本設定は 2,000 件で固定だからである。
--
-- 方針は 3 つ。
--   (1) scale_factor = 0 ＋ 実数 threshold: 発動条件を行数に比例させない。行数が増えても
--       発動間隔が伸びなくなる。実際の発動頻度の下限は autovacuum_naptime（既定 60 秒）が握るので、
--       threshold は「naptime 内に溜まるゴミ」より小さければ実質「毎 naptime」になる。
--   (2) fillfactor: ページに余白があり、かつ **index 対象列を変更しない** UPDATE は新版を同じページに
--       置ける（HOT update）。この場合 index を書き換えずに済む＝ index にゴミが出ない。
--       下の表のうち HOT が成立するのは concurrency_counter と scheduler_lease だけ（後述）。
--   (3) cost_delay = 0: 数ページのテーブルの vacuum は全速で走らせても I/O 負荷にならない。
--       大きいテーブルには設定せず既定の抑制を残す。
--   (4) vacuum_index_cleanup = on: **index 清掃のバイパスを止める。** PG14 以降の既定 AUTO は、
--       dead item が少ない（目安 2%）と判断すると vacuum が **index の清掃を丸ごと省略**する。
--       ここで狙っているのは partial index に溜まる古いエントリの回収なので、数百万行の表に対して
--       閾値 2,000 という「相対的にごく少量」の設定と AUTO を組み合わせると、autovacuum は毎回
--       起動するのに index が掃除されない、という最悪の組み合わせになる（Codex 指摘）。
--       下の実測は 20 万行に対し 18 万件の churn だったためこの条件を再現できていない。
--
-- 数値はすべて初期値であり、pg_stat_user_tables.n_dead_tup を見ながら運用で調整する
-- （engine.md の「本書の数値はすべて初期値」と同じ扱い）。

-- ---------------------------------------------------------------------------
-- 大〜中規模 × 局所更新: 比例条件を外すのが本命。fillfactor は設定しない。
-- これらは status / visible_at 等の **index 対象列**が更新で変わるため HOT が成立せず、
-- 余白を空けてもテーブルを膨らませるだけで index のゴミは減らないため。
-- ---------------------------------------------------------------------------

-- step_execution: 1 step の生涯で pending→ready→running→terminal ＋ リース heartbeat と、
-- 更新回数が最も多い。ready/lease の partial index が劣化すると claim に直撃する（#438）。
alter table step_execution set (
    vacuum_index_cleanup           = on,
    autovacuum_vacuum_scale_factor = 0,
    autovacuum_vacuum_threshold    = 2000,
    autovacuum_analyze_scale_factor = 0,
    autovacuum_analyze_threshold   = 5000
);

-- workflow_run: status 遷移・promote・timeout 回収。step_execution と同型で更新頻度は 1 桁低い。
alter table workflow_run set (
    vacuum_index_cleanup           = on,
    autovacuum_vacuum_scale_factor = 0,
    autovacuum_vacuum_threshold    = 1000,
    autovacuum_analyze_scale_factor = 0,
    autovacuum_analyze_threshold   = 2000
);

-- generation_run: chat 側の同型（claim/リース/heartbeat・crates/durable の共有パターン）。
-- 同じ機序で劣化するため同じ扱いにする。
alter table generation_run set (
    vacuum_index_cleanup           = on,
    autovacuum_vacuum_scale_factor = 0,
    autovacuum_vacuum_threshold    = 1000,
    autovacuum_analyze_scale_factor = 0,
    autovacuum_analyze_threshold   = 2000
);

-- job_queue: claim が visible_at を進め ack が DELETE する。1 ジョブ 1 サイクルで必ずゴミが出る。
-- visible_at は job_queue_claim_idx の対象列なので HOT にはならない。
alter table job_queue set (
    vacuum_index_cleanup           = on,
    autovacuum_vacuum_scale_factor = 0,
    autovacuum_vacuum_threshold    = 500,
    autovacuum_analyze_scale_factor = 0,
    autovacuum_analyze_threshold   = 1000
);

-- ---------------------------------------------------------------------------
-- 極小 × 極端な更新頻度: HOT update が成立するので fillfactor が効く。
-- vacuum は数ページで終わるので cost_delay も外す。
-- ---------------------------------------------------------------------------

-- concurrency_counter: 1 step あたり 6 回更新される（3 スコープ × acquire/release）。
-- step 実行の hot path にあり、行数は「テナント × スコープ」で数十〜数百に留まる。
-- 更新されるのは current_n / updated_at だけで PK は動かない ＝ **HOT update が成立する**。
-- 余白を厚めに取り、index を書き換えずページ内で版を回せるようにする。
alter table concurrency_counter set (
    fillfactor                     = 70,
    autovacuum_vacuum_scale_factor = 0,
    autovacuum_vacuum_threshold    = 100,
    autovacuum_vacuum_cost_delay   = 0
);

-- scheduler_lease: 単一行（id=1）を tick ごと（既定 5 秒）に UPDATE し続ける。
-- 更新列は owner / expires_at で PK は動かない ＝ HOT が成立する。1 行なので余白は最大に振る。
alter table scheduler_lease set (
    fillfactor                     = 50,
    autovacuum_vacuum_scale_factor = 0,
    autovacuum_vacuum_threshold    = 50,
    autovacuum_vacuum_cost_delay   = 0
);

-- ---------------------------------------------------------------------------
-- 既存データへ fillfactor を効かせる（運用手順・この migration ではやらない）
-- ---------------------------------------------------------------------------
--
-- `ALTER TABLE ... SET (fillfactor = ...)` は**既存のヒープページを書き換えない**。新しい余白は
-- 以降の INSERT/UPDATE で作られるページにしか効かないので、既に満杯のページに載っている行は
-- しばらく HOT update にならない（該当は上の 2 テーブル）。
--
-- 既に稼働中の DB で即座に効かせたい場合は、**別途**次を実行する。どちらも ACCESS EXCLUSIVE を
-- 取るため migration（トランザクション内）には置けない。対象はいずれも数ページなので一瞬で終わる。
--
--   VACUUM FULL concurrency_counter;
--   VACUUM FULL scheduler_lease;
--
-- 新規 DB では最初から新しい fillfactor でページが作られるので何もしなくてよい。
