-- カスタム指示（設定 → パーソナライズ・#900）。
--
-- 本人が登録する「自己紹介」と「答え方の希望」と「口調」。チャットの生成ワーカーが run の実行主体の
-- 行を読み、system プロンプトへ「ユーザー本人の希望」として区切って足す（chat::custom_instructions）。
--
-- 1 人 1 行（tenant_id ＋ org ＋ user_id）。範囲は AuthContext から決め、本人以外は読み書きしない。
-- 本文は個人の記述なので監査ログには載せない（文字数と口調だけを残す）。管理者向けの集計にも使わない。
-- 上限は API（chat::custom_instructions::MAX_CHARS）と同じ。DB でも縛り、上限を超えた行を作れなくする。
create table user_custom_instruction (
    tenant_id   text        not null,
    org         text        not null,
    user_id     text        not null,
    -- あなたについて（職種・担当・よく扱う資料など）。
    about       text        not null default '' check (char_length(about) <= 1500),
    -- 答え方の希望（結論を先に・出典を付ける など）。
    style       text        not null default '' check (char_length(style) <= 1500),
    tone        text        not null default 'default'
                    check (tone in ('default', 'formal', 'casual', 'concise')),
    updated_at  timestamptz not null default now(),
    primary key (tenant_id, org, user_id)
);
