-- ゴミ箱の保持期間（GAP-108c・D-202）。
--
-- 仕組みだけ入れ、既定は無効（human 判断・2026-11-11）。運用者がテナントごとに日数（1〜3650）を入れたときだけ、
-- ゴミ箱に入れてから日数を過ぎたものを、システムの主体（system:trash-retention）で完全に削除する
-- （storage::trash_retention・中断アップロードの回収タイマに相乗り）。null は「自動では消さない」。
--
-- 期限は greatest(削除した時刻, trash_retention_since) + 日数。日数を null から入れたときと短くしたときは
-- trash_retention_since を今にする（入れた直後・縮めた直後に、古いゴミ箱が一斉に消えない猶予）。長くしたとき・
-- null に戻したときは変えない。
--
-- 新しい表は作らない（tenant に列を足すだけ）ので、RLS の追加は要らない。
alter table tenant
    add column if not exists trash_retention_days int
        check (trash_retention_days between 1 and 3650),
    add column if not exists trash_retention_since timestamptz;

-- 保持期間の掃除が、テナントごとに古い削除の根から読む（テナントを横断して 1 時間ごと）。ゴミ箱に入っている
-- ノードだけの部分索引なので小さい。無いと、日数を入れたテナントの node を回ごとに全件読む。
create index if not exists node_trash_idx on node (tenant_id, deleted_at, id)
    where deleted_at is not null and system = false;
