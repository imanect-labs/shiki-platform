-- Phase 15 Task 15.6（追補）: 派生値の再配信（derived イベント）の発行を、元の変更イベント 1 件につき
-- 1 回に限る台帳。
--
-- 派生値の再配信はリレー（outbox の台帳 consumer）が元の変更を採番するときに積む。台帳 consumer の
-- 名前は設定で分けられる（配信系を独立させる・検証の隔離）ので、同じ変更を複数の consumer が見る。
-- 元のイベント id をここへ先に書けた 1 つのリレーだけが再配信を積む（一意制約で直列化・重複しない）。
-- 元のイベントが outbox の GC で消えたら、この行も消える。
create table data_stream_fanout (
    event_id   bigint      primary key
        references storage_event_outbox (id) on delete cascade,
    tenant_id  text        not null,
    created_at timestamptz not null default now()
);
