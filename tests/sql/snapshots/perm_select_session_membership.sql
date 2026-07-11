-- query: GetClocktowerGames
-- statement 1 of 1 (returns rows)
with temp_selected_clocktowerGame as (
select id, name
from clocktowerGames
where
 "clocktowerGames"."id" in (select value from json_each($session_activeClocktowerGameIds))

)

select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_clocktowerGame.id,
      'name', temp_selected_clocktowerGame.name
    )
  ), json('[]')) as clocktowerGame
from temp_selected_clocktowerGame

