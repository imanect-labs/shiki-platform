-- #816: jobq の claim をテナント間で公平にする（start-time fair queuing）。
--
-- 旧 claim は `ORDER BY id`（全キュー共通の到着順）で、1 テナントが大量投入すると後から来た
-- 他テナントのジョブは洪水が捌けるまで待たされた（rag_ingest は全レプリカで consumer 数本しか
-- 回らないので、1 社の一括アップロードが全社のインジェストを数時間止め得る）。
--
-- fair_seq = 「そのテナントの直前のジョブの次」と「キューの仮想時刻」の大きい方を投入時に振り
-- （jobq::enqueue_on）、claim は `ORDER BY fair_seq, id`。各テナントは fair_seq を 1 ジョブ 1 ずつ
-- しか進められないので、洪水テナントのジョブは先へ伸び、後から来たテナントは仮想時刻から始まって
-- **洪水と 1:1 で交互に**取り出される。テナント内の順序は保つ。式と理由は crates/jobq の冒頭。
--
-- 既存行・旧版の投入は 0（＝最優先で従来どおり id 順）。新旧混在中は旧版の claim が id 順で
-- 取るだけで、取りこぼし・二重配信は起きない（公平性が効かず、仮想時刻が進まないだけ）。

alter table job_queue add column fair_seq bigint not null default 0;

-- キューごとの仮想時刻（単調増加）。claim が確保した行の最大 fair_seq まで進める（下げない）。
-- キュー内の行から毎回求めると、バックオフ明け・クラッシュで古い fair_seq の行が可視に戻った
-- 瞬間に巻き戻り、後着テナントが先行テナントの残りを追い越せるため、値として保持する。
-- 行はキュー数だけ（テナントに依らない）。vt は索引に含めないので更新は HOT になり得る。
create table job_queue_vtime (
    queue text   primary key,
    vt    bigint not null default 0
);
alter table job_queue_vtime set (
    fillfactor                     = 50,
    autovacuum_vacuum_scale_factor = 0,
    autovacuum_vacuum_threshold    = 1000
);

-- claim の走査経路: キュー内を fair_seq 順に辿り、可視（visible_at <= now()）な先頭から確保する。
-- 読み飛ばすのは処理中・バックオフ中の行だけ（旧 claim は pkey を id 順に辿って**他キューの行も**
-- 読み飛ばしていた）。visible_at を索引に含めないので、claim / fail / defer の更新は HOT になり得る。
create index job_queue_fair_idx on job_queue (queue, fair_seq, id);

-- 投入時の「そのテナントの直前の fair_seq」の参照と、テナント消去（jobq::delete_tenant）の経路。
create index job_queue_tenant_idx on job_queue (tenant_id, queue, fair_seq);

-- 旧 claim 用の索引。旧 claim の実プランは pkey 走査でこれを使っておらず（EXPLAIN 実測・#816）、
-- 新 claim も使わない。visible_at を含むせいで claim の更新を毎回非 HOT にしていた。
drop index if exists job_queue_claim_idx;
