import type { SqlInfo } from './types';

export const sql: SqlInfo[] = [  {
    include: true,
    params: [ "session_context" ],
    sql: `with temp_selected_entry as (
select id
from entries
where
 "entries"."details" is jsonb($session_context)

)

select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_entry.id
    )
  ), json('[]')) as entry
from temp_selected_entry
`
  }
];
export const syncSql: SqlInfo[] | undefined = undefined;
