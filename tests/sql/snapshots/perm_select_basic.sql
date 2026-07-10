-- query: GetPosts
-- statement 1 of 1 (returns rows)
with temp_selected_post as (
select authorId, id, title
from posts
where
 "posts"."authorId" = $session_userId

)

select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_post.id,
      'title', temp_selected_post.title,
      'authorId', temp_selected_post.authorId
    )
  ), json('[]')) as post
from temp_selected_post

