-- 1 回答に紐づく権限判定と引用を監査ログから引く（#829・Task 8.1/8.2）。
--
-- チャットの回答のための検索（rag.search）・本文の読み出し（rag.doc_read）は、metadata に
-- 会話（thread_id）と回答（assistant のメッセージ・message_id）を持つ。監査ログ画面の
-- 「この回答の記録で絞る」「この会話の記録で絞る」はこの 2 つで引くので、持つ行だけの
-- 部分索引にする（ほかの行は載らないので小さい）。
--
-- 注: CONCURRENTLY にしない（0075 と同じ理由）。

create index if not exists audit_log_tenant_message_idx
    on audit_log (tenant_id, org, (metadata ->> 'message_id'), created_at desc, id desc)
    where metadata ? 'message_id';

create index if not exists audit_log_tenant_thread_idx
    on audit_log (tenant_id, org, (metadata ->> 'thread_id'), created_at desc, id desc)
    where metadata ? 'thread_id';
