-- アプリの能力の利用量に、呼んだ主体の種類を記録する（BUG-204）。
--
-- これまで B2 の自動実行（アプリの主体・BUG-183）の呼び出しは、user_sub にサービスアカウントの利用者の id を入れて
-- いたため、利用量の API（GET /apps/{app_id}/usage）で利用者の分と見分けられなかった。principal が 'app' の行は
-- アプリの主体の分で、user_sub は空（利用者ではない）。'user' の行は利用者（B1 の PKCE・B2 が交換したトークン）の分。
--
-- 既にある行のうち、インストールに今のサービスアカウントの利用者（b2_service_sub・migration 0141）と同じ user_sub の
-- ものは、アプリの主体の分に直す（入れ直す前の古いサービスアカウントの分は見分けられないので、そのまま残す）。

alter table app_capability_usage
    add column principal text not null default 'user';

update app_capability_usage u
   set principal = 'app', user_sub = ''
  from app_installation i
 where i.tenant_id = u.tenant_id
   and i.app_id = u.app_id
   and i.b2_service_sub is not null
   and u.user_sub = i.b2_service_sub;

-- 新しく書く行だけを確かめる（not valid。前からの行に空の user_sub が残っていても、適用を止めない）。
alter table app_capability_usage
    add constraint app_capability_usage_principal_check
        check ((principal = 'user' and user_sub <> '') or (principal = 'app' and user_sub = ''))
        not valid;
