-- query: CreatePost
-- statement 1 of 1 (returns rows)
insert into posts (title, content, authorId, published, updatedAt)
select "__pyre_proposed"."title", "__pyre_proposed"."content", "__pyre_proposed"."authorId", "__pyre_proposed"."published", "__pyre_proposed"."updatedAt"
from (
  select null as "id", $title as "title", $content as "content", $authorId as "authorId", $published as "published", unixepoch() as "updatedAt"
) as "__pyre_proposed"
where "__pyre_proposed"."authorId" = $session_userId returning json_object('title', "title", 'content', "content", 'authorId', "authorId", 'published', json(case when "published" = 1 then 'true' else 'false' end)) as "post", json_array(json_object('table_name', 'posts', 'headers', json_array('id', 'title', 'content', 'authorId', 'published', 'updatedAt'), 'rows', json_array(json_array("id", "title", "content", "authorId", "published", "updatedAt")))) as _affectedRows

