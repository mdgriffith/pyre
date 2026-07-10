-- query: CreateUserWithPosts
-- statement 1 of 11 (setup)
insert into users (name, status, status__reason, updatedAt)
values ($name, case when json_valid($status) then json_extract($status, '$._type') else $status end, case when json_valid($status) then json_extract($status, '$.reason') else null end, unixepoch())

-- statement 2 of 11 (setup)
drop table if exists temp_inserted_user

-- statement 3 of 11 (setup)
create temp table temp_inserted_user as
  select last_insert_rowid() as id

-- statement 4 of 11 (setup)
insert into posts (authorId, title, content, updatedAt)
  select "temp_inserted_user"."id", 'First Post', 'Hello', unixepoch()
  from temp_inserted_user

-- statement 5 of 11 (setup)
drop table if exists temp_inserted_firstPost

-- statement 6 of 11 (setup)
create temp table temp_inserted_firstPost as
  select t.rowid as id
  from "posts" t
  join "temp_inserted_user" p on t."authorId" = p."id"

-- statement 7 of 11 (setup)
insert into posts (authorId, title, content, updatedAt)
  select "temp_inserted_user"."id", 'Second Post', 'World', unixepoch()
  from temp_inserted_user

-- statement 8 of 11 (setup)
drop table if exists temp_inserted_secondPost

-- statement 9 of 11 (setup)
create temp table temp_inserted_secondPost as
  select t.rowid as id
  from "posts" t
  join "temp_inserted_user" p on t."authorId" = p."id"

-- statement 10 of 11 (returns rows)
select json_group_array(json(affected_row)) as _affectedRows
from (
  select json_object(
    'table_name', 'users',
    'headers', json_array('id', 'name', 'status', 'status__reason', 'updatedAt'),
    'rows', json_group_array(json_array("users"."id", "users"."name", "users"."status", "users"."status__reason", "users"."updatedAt"))
  ) as affected_row
  from "users"
  join temp_inserted_user temp_table on "users".rowid = temp_table.id
  union all
  select json_object(
    'table_name', 'posts',
    'headers', json_array('id', 'title', 'content', 'authorId', 'updatedAt'),
    'rows', json_group_array(json_array("posts"."id", "posts"."title", "posts"."content", "posts"."authorId", "posts"."updatedAt"))
  ) as affected_row
  from "posts"
  join temp_inserted_firstPost temp_table on "posts".rowid = temp_table.id
  union all
  select json_object(
    'table_name', 'posts',
    'headers', json_array('id', 'title', 'content', 'authorId', 'updatedAt'),
    'rows', json_group_array(json_array("posts"."id", "posts"."title", "posts"."content", "posts"."authorId", "posts"."updatedAt"))
  ) as affected_row
  from "posts"
  join temp_inserted_secondPost temp_table on "posts".rowid = temp_table.id
)

-- statement 11 of 11 (returns rows)
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

