-- query: SeedNull
-- statement 1 of 1 (returns rows)
insert into clocktowerLifecycles (id, end, status, updatedAt)
values ($id, null, 'running', unixepoch()) returning json_object('id', "id", 'end', json("end"), 'status', "status") as "clocktowerLifecycle", json_array(json_object('table_name', 'clocktowerLifecycles', 'headers', json_array('id', 'end', 'status', 'updatedAt'), 'rows', json_array(json_array("id", "end", "status", "updatedAt")))) as _affectedRows

-- query: GetNull
-- statement 1 of 1 (returns rows)
with temp_selected_clocktowerLifecycle as (
select end, id
from clocktowerLifecycles
where
 "clocktowerLifecycles"."end" is null

)

select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_clocktowerLifecycle.id,
      'end', json(temp_selected_clocktowerLifecycle.end)
    )
  ), json('[]')) as clocktowerLifecycle
from temp_selected_clocktowerLifecycle

-- query: EndLifecycle
-- statement 1 of 1 (returns rows)
update clocktowerLifecycles
set status = 'ended', updatedAt = unixepoch()
where
 "clocktowerLifecycles"."end" is null
 returning json_object('status', "status") as "clocktowerLifecycle", json_array(json_object('table_name', 'clocktowerLifecycles', 'headers', json_array('id', 'end', 'status', 'updatedAt'), 'rows', json_array(json_array("id", "end", "status", "updatedAt")))) as _affectedRows

-- query: DeleteNull
-- statement 1 of 1 (returns rows)
delete from clocktowerLifecycles
where
 "clocktowerLifecycles"."end" is null
 returning json_object() as "clocktowerLifecycle", json_array(json_object('table_name', 'clocktowerLifecycles', 'headers', json_array('id', 'end', 'status', 'updatedAt'), 'rows', json_array(json_array("id", "end", "status", "updatedAt")))) as _affectedRows

