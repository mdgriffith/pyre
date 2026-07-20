-- query: GetUserGraph
-- statement 1 of 1 (returns rows)
with temp_selected_user as (
    select id
    from users

), temp_selected_user__writings as (
    select
      t.authorId,
      jsonb_group_array(jsonb_object(
        'id', t.id,
        'title', t.title
      )) as writings
    from posts t
    where t.authorId in (select id from temp_selected_user)
    group by t.authorId
    order by t.authorId
), temp_selected_user__accounts as (
    select
      t.userId,
      jsonb_group_array(jsonb_object(
        'id', t.id,
        'name', t.name
      )) as accounts
    from accounts t
    where t.userId in (select id from temp_selected_user)
    group by t.userId
    order by t.userId
)
select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_user.id,
      'writings', coalesce(temp__posts.writings, jsonb('[]')),
      'accounts', coalesce(temp__accounts.accounts, jsonb('[]'))
    )
  ), json('[]')) as user
from temp_selected_user
  left join temp_selected_user__writings temp__posts on temp__posts.authorId = temp_selected_user.id
  left join temp_selected_user__accounts temp__accounts on temp__accounts.userId = temp_selected_user.id

