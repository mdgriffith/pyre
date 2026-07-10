-- query: IntOperators
-- statement 1 of 1 (returns rows)
with temp_selected_user as (
select id
from users
where
 ("users"."id" > 0 and "users"."id" < 100 and "users"."id" >= 2 and "users"."id" <= 99)

)

select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_user.id
    )
  ), json('[]')) as user
from temp_selected_user

