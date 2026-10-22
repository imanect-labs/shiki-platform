-- #816: jobq の claim をテナント間で公平にする（start-time fair queuing）。
--
-- 旧 claim は `ORDER BY id`（全キュー共通の到着順）で、1 テナントが大量投入すると後から来た
-- 他テナントのジョブは洪水が捌けるまで待たされた（rag_ingest は全レプリカで consumer 数本しか
-- 回らないので、1 社の一括アップロードが全社のインジェストを数時間止め得る）。
--
-- fair_seq = 「そのテナントの直前のジョブの次」と「仮想時刻（可視な行の最小 fair_seq）」の大きい方を
-- 投入時に振る（jobq::enqueue_on。fail / defer の再配信でも振り直す。式と理由は crates/jobq 冒頭）。claim は `ORDER BY fair_seq, id`。各テナントは fair_seq を
-- 1 ジョブ 1 ずつしか進められないので、洪水テナントのジョブは fair_seq が先へ伸び、後から来た
-- テナントは仮想時刻から始まって**洪水と 1:1 で交互に**取り出される。テナント内の順序は保つ。
--
-- 既存行・旧版の投入は 0（＝最優先で従来どおり id 順）。新旧混在中は旧版の claim が id 順で
-- 取るだけで、取りこぼし・二重配信は起きない（公平性が効かないだけ）。

alter table job_queue add column fair_seq bigint not null default 0;

-- claim の走査経路: キュー内を fair_seq 順に辿り、可視（visible_at <= now()）な先頭から確保する。
-- 読み飛ばすのは処理中・バックオフ中の行だけ（旧 claim は pkey を id 順に辿って**他キューの行も**
-- 読み飛ばしていた）。visible_at を索引に含めないので、claim の更新は HOT になり得る
-- （fail / defer は fair_seq を振り直すので HOT にはならない）。
create index job_queue_fair_idx on job_queue (queue, fair_seq, id);

-- 投入時の「そのテナントの直前の fair_seq」の参照と、テナント消去（jobq::delete_tenant）の経路。
create index job_queue_tenant_idx on job_queue (tenant_id, queue, fair_seq);

-- 旧 claim 用の索引。旧 claim の実プランは pkey 走査でこれを使っておらず（EXPLAIN 実測・#816）、
-- 新 claim も使わない。visible_at を含むせいで claim の更新を毎回非 HOT にしていた。
drop index if exists job_queue_claim_idx;
