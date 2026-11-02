-- B2 の自動実行（event・cron）の関数の直近の失敗を、作成者が見られる場所に残す（GAP-60 の残り）。
--
-- これまで自動実行の失敗（シークレット・トークン・実行の誤り・関数の失敗）は、サーバーのログに warn で書くだけだった。
-- 関数ごとに 1 行で、行があるのは「その関数の最後の自動実行が失敗した」とき。次に成功したら行を消す（実行のたびに
-- 行を足さない。一括取込の直後はイベントの実行が数万回続くので、履歴にすると際限なく増える）。
--
-- message は作成者に見せてよい文だけ（app-platform の function_failure と同じ整え方。スタックの行・内部の文・URL・
-- ほかの人の情報を含まない）。インストールの一覧（GET /apps/installations）は、アプリの持ち主にだけ返す。
-- アンインストール・入れ直しで消す。

create table app_function_failure (
    tenant_id       text        not null,
    org             text        not null,
    app_id          uuid        not null,
    function        text        not null,
    -- 失敗した自動実行のきっかけ（event・cron）。
    trigger         text        not null check (trigger in ('event', 'cron')),
    -- 理由（作成者が投げた誤りは script_error など・FunctionFailure.code と同じ語彙）。
    code            text        not null,
    message         text        not null,
    -- 最後に失敗した時刻と、続けて失敗し始めた時刻・続けて失敗した回数。
    failed_at       timestamptz not null default now(),
    first_failed_at timestamptz not null default now(),
    failures        integer     not null default 1,
    primary key (tenant_id, app_id, function)
);
