-- query: GetUsers
-- statement 1 of 1 (returns rows)
with temp_selected_user as (
select id, status, status__reason
from users

)

select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_user.id,
      'status', 
      case
        when temp_selected_user.status = 'Active' then json_object('_type', 'Active')
        when temp_selected_user.status = 'Inactive' then json_object('_type', 'Inactive')
        when temp_selected_user.status = 'Special' then
          json_object(
            '_type', 'Special',
            'reason', temp_selected_user.status__reason
          )
      end
    )
  ), json('[]')) as user
from temp_selected_user

