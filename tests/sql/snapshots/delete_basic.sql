-- query: DeleteUser
-- statement 1 of 5 (setup)
drop table if exists temp_deleted_users

-- statement 2 of 5 (setup)
create temp table temp_deleted_users as select * from "users" where
 "users"."id" = $id

-- statement 3 of 5 (setup)
delete from users
where
 "users"."id" = $id

-- statement 4 of 5 (returns rows)
select json_group_array(json(affected_row)) as _affectedRows
from (
  select json_object(
    'table_name', 'users',
    'headers', json_array('id', 'name', 'status', 'updatedAt'),
    'rows', json_group_array(json_array(temp_deleted_users."id", temp_deleted_users."name", temp_deleted_users."status", temp_deleted_users."updatedAt"))
  ) as affected_row
  from temp_deleted_users
)

-- statement 5 of 5 (returns rows)
select
  coalesce(json_group_array(
    json_object(

    )
  ), json('[]')) as user
from temp_deleted_users

