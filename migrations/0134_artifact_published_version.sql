-- ミニアプリの下書きと公開を分ける（D-16 の (a)・GAP-36）。
--
-- これまでは保存した瞬間に、共有したすべての人の画面とボタンの実行先が新しい版に変わった。保存は下書きの版
-- （artifact_version の追記）のままにし、使う人（editor でない viewer）に出す版を published_version に持つ。
-- editor は下書きを試せ、版を選んで公開する。公開し直すと、それまでの公開した版を previous_published_version に
-- 残す（公開を戻す先）。published_version が null のアプリは、まだ公開していない（使う人は開けない）。
--
-- 下書きと公開を分ける種類は artifact::ArtifactKind::has_drafts（いまは mini_app だけ）。ほかの種類は列を使わない。

alter table artifact
    add column published_version          bigint,
    add column previous_published_version bigint,
    add column published_at               timestamptz,
    -- 公開した人（principal.id）。
    add column published_by               text;

-- 保存済みのミニアプリは、いまの版を公開した版として扱う（移行の前と同じく、使う人には今の版が見える）。
update artifact
   set published_version = current_version,
       published_at      = updated_at
 where kind = 'mini_app';

alter table artifact
    add constraint artifact_published_version_range check (
        published_version is null
        or (published_version >= 1 and published_version <= current_version)
    ),
    add constraint artifact_previous_published_version_range check (
        previous_published_version is null
        or (previous_published_version >= 1 and previous_published_version <= current_version)
    );
