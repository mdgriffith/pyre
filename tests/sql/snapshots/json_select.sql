-- query: GetEvents
-- statement 1 of 1 (returns rows)
with temp_selected_event as (
select counts, id, name, payload, tags
from events

)

select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_event.id,
      'name', temp_selected_event.name,
      'payload', json(temp_selected_event.payload),
      'tags', json(temp_selected_event.tags),
      'counts', json(temp_selected_event.counts)
    )
  ), json('[]')) as event
from temp_selected_event

