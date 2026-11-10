-- 判断に添えるコメントの鍵に作者を足す（BUG-47・GAP-08）。
--
-- コメントは遷移の後の rev の鍵で置く。鍵が (approval_id, rev) だけだと、ある担当者が置いたまま外し損ねた
-- コメントが、次の rev の枠を別の担当者からも塞ぎ、正しい承認者がコメント付きで判断しても受け付けられなく
-- なっていた。鍵に作者を含めれば、置き残しがあっても塞ぐのは本人の同じ rev の枠だけになる。
--
-- 起床時の照合（遷移の後の rev・遷移した人＝作者・判断の種別が一致するものだけを採る）は変えない。
-- 判断 API は、置く → 遷移 → 失敗なら外す、を台帳の行のロックを持った 1 つの Tx で行い、遷移に成功した
-- 判断のコメントだけを commit する（失敗・要求の取り消しでは何も残さない）。
alter table workflow_approval_comment drop constraint workflow_approval_comment_pkey;
alter table workflow_approval_comment add primary key (tenant_id, approval_id, rev, author);
