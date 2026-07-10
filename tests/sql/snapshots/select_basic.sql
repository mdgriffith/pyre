-- query: GetUsers
-- statement 1 of 1 (returns rows)
with temp_selected_user as (
select id, name
from users

)

select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_user.id,
      'name', temp_selected_user.name
    )
  ), json('[]')) as user
from temp_selected_user

