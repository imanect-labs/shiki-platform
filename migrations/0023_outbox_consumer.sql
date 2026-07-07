-- P10-A0 追補: outbox コンシューマの登録台帳（初回バックログスキップを一度きりにする）。
--
-- register_consumer は「有効化前のバックログを配送済みに刻んで一斉再配送を防ぐ」が、これを**毎起動**
-- 行うとサーバ停止中に到着した未配送イベントまで配送済みにされ取りこぼす（restart で消える）。
-- 本台帳に consumer 名を一度だけ記録し、**初回登録時のみ**バックポインタ（fast-forward）を打つ。
create table outbox_consumer (
    name          text        primary key,
    registered_at timestamptz not null default now()
);
