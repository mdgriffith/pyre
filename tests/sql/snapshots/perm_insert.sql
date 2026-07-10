-- query: CreatePost
-- statement 1 of 5 (setup)
insert into posts (title, content, authorId, published, updatedAt)
values ($title, $content, $authorId, $published, unixepoch())

-- statement 2 of 5 (setup)
drop table if exists temp_inserted_post

-- statement 3 of 5 (setup)
create temp table temp_inserted_post as
  select last_insert_rowid() as id

-- statement 4 of 5 (returns rows)
select json_group_array(json(affected_row)) as _affectedRows
from (
  select json_object(
    'table_name', 'posts',
    'headers', json_array('id', 'title', 'content', 'authorId', 'published', 'updatedAt'),
    'rows', json_group_array(json_array("posts"."id", "posts"."title", "posts"."content", "posts"."authorId", "posts"."published", "posts"."updatedAt"))
  ) as affected_row
  from "posts"
  join temp_inserted_post temp_table on "posts".rowid = temp_table.id
)

-- statement 5 of 5 (returns rows)
select
  coalesce(json_group_array(
    json_object(
      'title', t."title",
      'content', t."content",
      'authorId', t."authorId",
      'published', json(case when t."published" = 1 then 'true' else 'false' end)
    )
  ), json('[]')) as post
from posts t
join temp_inserted_post temp_table on t.rowid = temp_table.id

