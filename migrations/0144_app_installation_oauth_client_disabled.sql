-- 起動時のクライアントの照合で、Keycloak で無効にされていた B2 のクライアントを有効に戻さず、印を付ける（BUG-201・D-137）。
--
-- 照合（migration 0137・BUG-172）は、有効なインストールのクライアントを今の登録の形（有効）にそろえる。B2 は secret を
-- 持つので、管理者が漏れた secret を止めるために Keycloak で無効にしたのなら、同じ secret のまま有効に戻すとアプリの主体の
-- トークンを再び取れてしまう。そのため B2 が無効なら有効に戻さず、この列を true にし、インストールの画面で知らせる
-- （B1 は secret を持たないので、これまでどおり有効に戻す）。照合は true の行を起動のたびに試し直し、管理者が有効に
-- 戻していたら今の形にそろえて false に戻す。インストール（入れ直し）のときも false に戻す。

alter table app_installation add column oauth_client_disabled boolean not null default false;
