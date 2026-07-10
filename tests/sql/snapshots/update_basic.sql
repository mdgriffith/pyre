-- query: RenameUser
-- statement 1 of 3 (setup)
update users
set name = $name, updatedAt = unixepoch()
where
 "users"."id" = $id
 returning *

-- statement 2 of 3 (returns rows)
select json_group_array(json(affected_row)) as _affectedRows
from (
  select json_object(
    'table_name', 'users',
    'headers', json_array('id', 'name', 'status', 'status__reason', 'updatedAt'),
    'rows', json_group_array(json_array(t."id", t."name", t."status", t."status__reason", t."updatedAt"))
  ) as affected_row
  from "users" t
where
 t."id" = $id
)

-- statement 3 of 3 (returns rows)
select
  coalesce(json_group_array(
    json_object(
      'name', t."name"
    )
  ), json('[]')) as user
from "users" t
where
 t."id" = $id

