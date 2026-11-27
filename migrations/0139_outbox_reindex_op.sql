-- 書込イベントの outbox に、社内文書の検索への取り込み直しの依頼（op = 'reindex'）を足す（D-126・GAP-51）。
--
-- 取り込みの記録が無いファイル（検索を有効にする前に置いた・前の版の不具合で取り込みを飛ばした）と、取り込みに
-- 失敗したファイルを、いまの内容のバージョンで取り込み直す。依頼は社内文書の検索（rag）が、取り込みの状態を
-- 確かめた Tx の中で outbox に積み、relay → job_queue → 取り込み（rag_ingest_job の冪等キー）の既存の経路を通す。
-- 内容もメタも変わらないので、node.version は進めない。
--
-- ほかの consumer（ワークフローのきっかけ・ミニアプリの関数・購読）は op を許可した値で絞っているので、
-- このイベントは読み飛ばす。
--
-- 制約を広げるだけなので、既存の行はすべて新しい制約を満たす（足すときの検証は outbox の行を 1 回読む）。
alter table storage_event_outbox drop constraint storage_event_outbox_op_check;
alter table storage_event_outbox add constraint storage_event_outbox_op_check check (op in (
    'create', 'update', 'rename', 'move', 'delete', 'restore', 'reindex'
));
