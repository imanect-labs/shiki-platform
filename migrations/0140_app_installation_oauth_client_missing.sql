-- 起動時のクライアントの照合で、Keycloak に作り直せないクライアントがあったインストールに印を付ける（GAP-70）。
--
-- 照合（migration 0137・BUG-172）は、Keycloak に無くなったアプリのクライアントを作り直す。B1 はそのまま作り、B2 は
-- 保管してある secret（secrets・アプリの主体の can_use）で作る（新しい secret を作らないので保管し直しが要らない）。
-- secret を読めない（secrets が未構成・secret が無い）とき、クライアントの id が インストールの決まった形
-- （app-<app_id>-b1 / -b2）でないときは作れない。そのときこの列を true にし、インストールの画面で入れ直しが要ると
-- 知らせる。照合は true の行を起動のたびに試し直し、作れたら false に戻す。インストールのときも false に戻す。

alter table app_installation add column oauth_client_missing boolean not null default false;
