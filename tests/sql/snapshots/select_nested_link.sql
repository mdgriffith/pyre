-- query: GetUsersWithPosts
-- statement 1 of 1 (returns rows)
with temp_selected_user as (
    select id, name
    from users

), temp_selected_posts as (
    select
      t.authorId,
      jsonb_group_array(jsonb_object(
        'id', t.id,
        'title', t.title
      )) as posts
    from posts t
    where t.authorId in (select id from temp_selected_user)
    group by t.authorId
    order by t.authorId
)
select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_user.id,
      'name', temp_selected_user.name,
      'posts', coalesce(temp__posts.posts, jsonb('[]'))
    )
  ), json('[]')) as user
from temp_selected_user
  left join temp_selected_posts temp__posts on temp__posts.authorId = temp_selected_user.id

