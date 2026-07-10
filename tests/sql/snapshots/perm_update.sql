-- query: UpdateArticle
-- statement 1 of 1 (returns rows)
update articles
set title = $title, updatedAt = unixepoch()
where
 ("articles"."id" = $id and "articles"."authorId" = $session_userId)
 returning json_object('title', "title") as "article", json_array(json_object('table_name', 'articles', 'headers', json_array('id', 'title', 'content', 'authorId', 'status', 'updatedAt'), 'rows', json_array(json_array("id", "title", "content", "authorId", "status", "updatedAt")))) as _affectedRows

