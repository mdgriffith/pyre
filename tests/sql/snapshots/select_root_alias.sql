-- query: GetPeople
-- statement 1 of 1 (returns rows)
with temp_selected_people as (
select id, name
from users

)

select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_people.id,
      'name', temp_selected_people.name
    )
  ), json('[]')) as people
from temp_selected_people
