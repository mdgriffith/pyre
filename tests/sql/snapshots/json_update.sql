-- query: UpdateEvent
-- statement 1 of 3 (setup)
update events
set payload = jsonb($payload), updatedAt = unixepoch()
where
 "events"."id" = $id
 returning *

-- statement 2 of 3 (returns rows)
select json_group_array(json(affected_row)) as _affectedRows
from (
  select json_object(
    'table_name', 'events',
    'headers', json_array('id', 'name', 'payload', 'tags', 'counts', 'updatedAt'),
    'rows', json_group_array(json_array(t."id", t."name", t."payload", t."tags", t."counts", t."updatedAt"))
  ) as affected_row
  from "events" t
where
 t."id" = $id
)

-- statement 3 of 3 (returns rows)
select
  coalesce(json_group_array(
    json_object(
      'payload', json(t."payload")
    )
  ), json('[]')) as event
from "events" t
where
 t."id" = $id

