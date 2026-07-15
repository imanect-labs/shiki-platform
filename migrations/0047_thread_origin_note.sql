-- issue #282: ノート×会話を 1:N にする（ノート由来スレッドの明示）。
--
-- ノートの分割ビュー（アシスタントパネル）から作られたスレッドに、由来ノートの id を
-- 保持する。これにより (1) サイドバーのチャット履歴で「ノート由来」と分かり、当該ノートへ
-- 辿れる、(2) 同一ノートに複数スレッドを関連付け（会話リセットで新スレッド・旧スレッドは
-- 履歴に残る）、(3) ノート側の「会話一覧」を単一真実源（この列）で引ける。
--
-- 表示名（origin_note_name）はスレッド作成時点の名前を非正規化して持つ（履歴一覧を
-- ノート doc の読み込み無しに描くため）。ノートのリネームには追随しない（履歴表示用の目安）。
-- 通常チャット由来のスレッドは両列 NULL（現行挙動そのまま）。
--
-- 注: これは表示・グルーピング用のメタで、認可の真実源ではない。ノート編集権とスレッド
-- 閲覧権は別 ReBAC・fail-closed のまま（暗黙共有しない）。
alter table thread add column if not exists origin_note_id uuid;
alter table thread add column if not exists origin_note_name text;

-- ノート側の会話一覧引き（テナント内・当該ノート・未削除・更新日降順）を効かせる。
-- 注: CodeRabbit は CONCURRENTLY を提案したが、本リポジトリの CI（Coverage ジョブ）は DB IT を
-- バイナリ跨ぎで**並列**に同一 DB へ流し各々 `sqlx::migrate!` を走らせるため、no-transaction な
-- CREATE INDEX CONCURRENTLY は他バイナリの transaction と advisory lock で**デッドロック**する。
-- transaction 内の通常 CREATE INDEX は migrator の advisory lock で直列化され安全。thread は
-- 小さく index 構築の書込ロックは一瞬なので、ここでは通常の CREATE INDEX を採る。
create index if not exists thread_origin_note_idx
    on thread (tenant_id, org, origin_note_id, updated_at desc)
    where origin_note_id is not null and deleted_at is null;
