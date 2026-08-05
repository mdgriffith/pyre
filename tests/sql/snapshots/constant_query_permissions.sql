-- query: GetPosts
-- statement 1 of 1 (returns rows)
with temp_selected_visiblePost as (
select id
from visiblePosts

)

select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_visiblePost.id
    )
  ), json('[]')) as visiblePost
from temp_selected_visiblePost

-- query: GetPosts
-- statement 1 of 1 (returns rows)
with temp_selected_hiddenPost as (
select id
from hiddenPosts
where
 0

)

select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_hiddenPost.id
    )
  ), json('[]')) as hiddenPost
from temp_selected_hiddenPost
