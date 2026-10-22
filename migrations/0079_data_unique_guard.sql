-- Task 15.3: 旧方式の一意な式インデックスの撤去（0078）で、一意性の強制に空白期間を作らない。
--
-- unique 宣言のあるテーブルのうち、その一意索引（data_unique・台帳 index_table = 4）が active でないもの
-- （0078 の時点でバックフィル途中だったテーブルなど）は、台帳を作り直させる。台帳を消して
-- pivot_index_init = false に戻すと、最初のアクセス（読取・書込のどちらでも）または常駐ワーカーの
-- スイープが data_table 行を FOR UPDATE で押さえて台帳を初期化し、unique を**同じ Tx で同期構築**して
-- active にしてから書込を通す（重複があれば初期化が 409 で止まり、書込は通らない）。
-- 加えて書込経路は、宣言された unique が active でなければ書込を拒否する（index/state.rs の lock_plan）。
with need as (
    select distinct d.tenant_int, d.table_int
      from data_table d
     cross join lateral jsonb_array_elements(coalesce(d.schema -> 'fields', '[]'::jsonb)) f
     where d.pivot_index_init
       and coalesce((f ->> 'unique')::boolean, false)
       and not exists (
           select 1 from data_index_state s
            where s.tenant_int = d.tenant_int and s.table_int = d.table_int
              and s.field_id = (f ->> 'id')::int and s.index_table = 4 and s.state = 'active')
)
, reset as (
    update data_table d set pivot_index_init = false
      from need n
     where d.tenant_int = n.tenant_int and d.table_int = n.table_int
    returning d.tenant_int, d.table_int
)
, gone as (
    delete from data_index_state s using reset r
     where s.tenant_int = r.tenant_int and s.table_int = r.table_int
    returning 1
)
delete from data_index_backfill b using reset r
 where b.tenant_int = r.tenant_int and b.table_int = r.table_int;
