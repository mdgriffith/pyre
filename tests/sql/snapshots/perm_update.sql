-- query: UpdateArticle
-- statement 1 of 2 (returns rows)
select json_array(json_insert(json_object('table_name', 'articles', 'primary_key', 'id', 'headers', json_array('id', 'title', 'content', 'authorId', 'status', 'updatedAt'), 'rows', json_array(json_array("id", "title", "content", "authorId", "status", "updatedAt"))), '$.headers[#]', '_pyre_preimage', '$.rows[0][#]', json('true'))) as _affectedRows from articles
where
 ("articles"."id" = $id and "articles"."authorId" = $session_userId)

-- statement 2 of 2 (returns rows)
update articles
set title = $title, updatedAt = unixepoch()
where
 ("articles"."id" = $id and "articles"."authorId" = $session_userId)
 returning json_object('title', "title") as "article", json_array(json_object('table_name', 'articles', 'primary_key', 'id', 'headers', json_array('id', 'title', 'content', 'authorId', 'status', 'updatedAt'), 'rows', json_array(json_array("id", "title", "content", "authorId", "status", "updatedAt")))) as _affectedRows

