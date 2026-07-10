-- query: CreatePost
-- statement 1 of 1 (returns rows)
insert into posts (title, content, authorId, published, updatedAt)
values ($title, $content, $authorId, $published, unixepoch()) returning json_object('title', "title", 'content', "content", 'authorId', "authorId", 'published', json(case when "published" = 1 then 'true' else 'false' end)) as "post", json_array(json_object('table_name', 'posts', 'headers', json_array('id', 'title', 'content', 'authorId', 'published', 'updatedAt'), 'rows', json_array(json_array("id", "title", "content", "authorId", "published", "updatedAt")))) as _affectedRows

