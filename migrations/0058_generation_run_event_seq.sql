-- イベント seq の採番カウンタを run 行に持たせる（並行追記の PK 衝突を根治する）。
--
-- 従来は追記のたびに `max(seq) + 1` を数えていた。同一 run へ**同時に**追記する経路
-- （親のトークン列と、サブエージェントのツールイベント中継）が並ぶと、READ COMMITTED では
-- 行ロックを取っても `max(seq)` を数える側は古いスナップショットのままなので、両者が同じ
-- seq を得て `generation_event_pkey` 違反で片方が落ちる（実 LLM の deep research run で
-- 発生し、run ごと失敗した）。
--
-- カウンタを run 行に置き、`UPDATE ... RETURNING` で採る。ロック対象の行そのものを読むので
-- EvalPlanQual により確定値が返り、スナップショットの古さに影響されない。
ALTER TABLE generation_run
    ADD COLUMN IF NOT EXISTS event_seq bigint NOT NULL DEFAULT 0;

-- 既存 run のカウンタを実際の最大 seq へ合わせる（合わせないと再開時に既存 seq と衝突する）。
UPDATE generation_run r
SET event_seq = COALESCE(
        (SELECT max(e.seq) FROM generation_event e WHERE e.run_id = r.run_id),
        0
    )
WHERE r.event_seq = 0;
