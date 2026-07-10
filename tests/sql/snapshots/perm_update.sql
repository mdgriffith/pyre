-- query: UpdateArticle
-- statement 1 of 3 (setup)
update articles
set title = $title, updatedAt = unixepoch()
where
 ("articles"."id" = $id and "articles"."authorId" = $session_userId)
 returning *

-- statement 2 of 3 (returns rows)
select json_group_array(json(affected_row)) as _affectedRows
from (
  select json_object(
    'table_name', 'articles',
    'headers', json_array('id', 'title', 'content', 'authorId', 'status', 'updatedAt'),
    'rows', json_group_array(json_array(t."id", t."title", t."content", t."authorId", t."status", t."updatedAt"))
  ) as affected_row
  from "articles" t
where
 (t."id" = $id and t."authorId" = $session_userId)
)

-- statement 3 of 3 (returns rows)
select
  coalesce(json_group_array(
    json_object(
      'title', t."title"
    )
  ), json('[]')) as article
from "articles" t
where
 (t."id" = $id and t."authorId" = $session_userId)

