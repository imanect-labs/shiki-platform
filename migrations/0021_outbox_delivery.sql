-- P10-A0: outbox の per-consumer 配送台帳（fan-out 点・roadmap/phase-10.md）。
-- ---------------------------------------------------------------------------
-- 背景: storage_event_outbox は単一 processed_at ack で、RAG relay が破壊的に消費している。
-- workflow-engine が storage.write の 2 人目のコンシューマになると、片方が処理済みにした瞬間に
-- 他方が取りこぼす。design §4.3 は outbox を fan-out 点と謳うが、実装は単一コンシューマのまま。
--
-- 方式（approach a = 配送台帳・単純 last_seq カーソルは採らない）:
--   各コンシューマは「自分がまだ配送していない outbox 行」を
--   `NOT EXISTS (SELECT 1 FROM outbox_delivery WHERE consumer = :me AND event_id = o.id)`
--   ＋ `FOR UPDATE SKIP LOCKED` で claim → 配送 → delivery 行を追記する。
--   存在性（NOT EXISTS）ベースの claim なので id 順・コミット順に依存せず、後から
--   コミットした小さい id の行も次スキャンで拾える（未コミット飛び越し問題を回避）。
--   outbox 行は全 consumer の delivery が揃った後に GC する（＋retention backstop）。
--
-- 既存 RAG relay は processed_at 経路のまま温存（挙動・テスト不変）。本台帳は追加コンシューマ
-- （workflow event matcher）のためのもので、生成側（emit_on）は一切変更しない＝fan-out 点として機能。
-- ---------------------------------------------------------------------------
create table outbox_delivery (
    consumer     text        not null,               -- コンシューマ名（例: 'workflow'）
    event_id     bigint      not null
        references storage_event_outbox (id) on delete cascade,
    tenant_id    text        not null,               -- GC/監査の絞り込み用（イベントから写す）
    delivered_at timestamptz not null default now(),
    primary key (consumer, event_id)
);

-- claim の anti-join（NOT EXISTS(delivery for me)）と GC を支える索引。
create index outbox_delivery_event_idx on outbox_delivery (event_id);
