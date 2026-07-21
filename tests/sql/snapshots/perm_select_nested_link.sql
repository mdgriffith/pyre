-- query: GetPostsWithComments
-- statement 1 of 1 (returns rows)
with temp_selected_post as (
    select id, title
    from posts
where
 "posts"."authorId" = $session_userId

), temp_selected_post__comments as (
    select
      t.postId,
      jsonb_group_array(jsonb_object(
        'id', t.id,
        'content', t.content
      )) as comments
    from comments t
where
 t."authorId" = $session_userId
    and t.postId in (select id from temp_selected_post)
    group by t.postId
    order by t.postId
)
select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_post.id,
      'title', temp_selected_post.title,
      'comments', coalesce(temp__comments.comments, jsonb('[]'))
    )
  ), json('[]')) as post
from temp_selected_post
  left join temp_selected_post__comments temp__comments on temp__comments.postId = temp_selected_post.id

