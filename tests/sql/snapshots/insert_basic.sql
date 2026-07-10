-- query: CreateUser
-- statement 1 of 5 (setup)
insert into users (name, status, updatedAt)
values ($name, 'Active', unixepoch())

-- statement 2 of 5 (setup)
drop table if exists temp_inserted_user

-- statement 3 of 5 (setup)
create temp table temp_inserted_user as
  select last_insert_rowid() as id

-- statement 4 of 5 (returns rows)
select json_group_array(json(affected_row)) as _affectedRows
from (
  select json_object(
    'table_name', 'users',
    'headers', json_array('id', 'name', 'status', 'status__reason', 'updatedAt'),
    'rows', json_group_array(json_array("users"."id", "users"."name", "users"."status", "users"."status__reason", "users"."updatedAt"))
  ) as affected_row
  from "users"
  join temp_inserted_user temp_table on "users".rowid = temp_table.id
)

-- statement 5 of 5 (returns rows)
select
  coalesce(json_group_array(
    json_object(
      'name', t."name",
      'status', 
      case
        when t.status = 'Active' then json_object('_type', 'Active')
        when t.status = 'Inactive' then json_object('_type', 'Inactive')
        when t.status = 'Special' then
          json_object(
            '_type', 'Special',
            'reason', t.status__reason
          )
      end
    )
  ), json('[]')) as user
from users t
join temp_inserted_user temp_table on t.rowid = temp_table.id

