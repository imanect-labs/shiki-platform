-- Phase 10 Task 10.10: effect_journal に reserved_at を追加（孤児予約の回収）。
--
-- 予約（check の Proceed）後・record 前にワーカーがクラッシュすると result_summary が NULL のまま
-- 残り、以降の check は永久に InProgress を返して step が詰まる。reserved_at を基準に、リース失効相当の
-- 時間が過ぎた孤児予約を別ワーカーが条件つき UPDATE で再取得できるようにする（回収窓のみ at-least-once・
-- Stage A の journal 対象 storage.write / workflow.start は冪等キーで重複排除される）。
alter table effect_journal
    add column reserved_at timestamptz not null default now();
