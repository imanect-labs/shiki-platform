-- 会話の分岐（発話の編集で枝を作り、同じスレッドで切り替える・#1166）。
--
-- message.parent_id を木として使う。これまでは assistant の親（＝応答した user 発話）だけを
-- 持ち、user 発話の親は NULL で、取得は作成順の線形だった。以後は user 発話の親を
-- 「その発話を送ったときに表示していた枝の葉（直前の assistant）」にする。過去の発話を
-- 編集すると、元の発話と同じ親を持つ兄弟の user 発話ができる（＝枝）。
--
-- thread.active_message_id は表示中の枝の葉。NULL は「最も新しいメッセージ」（従来どおり）。

-- 既存の user 発話の親を、同じスレッドで直前に作られた assistant で補う（作成順の線形を木へ）。
-- 発話と応答の枠は同じトランザクションで作られ created_at が同値なので、厳密に前のものを取る
-- （自分への応答を親にしない）。最初の発話は根（NULL のまま）。
update message u
   set parent_id = (
       select a.id
         from message a
        where a.thread_id = u.thread_id
          and a.tenant_id = u.tenant_id
          and a.role = 'assistant'
          and a.created_at < u.created_at
        order by a.created_at desc, a.id desc
        limit 1)
 where u.role = 'user'
   and u.parent_id is null;

-- 外部キーは張らない: thread → message → thread の循環になり、スレッドの物理削除（cascade）が
-- 削除中の行を更新しに行く。指す先が無い（消えた・別スレッド）値はアプリが「指定なし」として扱う。
alter table thread add column active_message_id uuid;

-- 枝の木を引く（スレッドのメッセージの id・親・作成時刻だけ）。親から子を引く用途にも使う。
create index if not exists message_thread_parent_idx on message (thread_id, parent_id);
