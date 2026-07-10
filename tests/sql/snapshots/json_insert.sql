-- query: CreateEvent
-- statement 1 of 5 (setup)
insert into events (name, payload, tags, counts, updatedAt)
values ($name, jsonb($payload), jsonb($tags), jsonb($counts), unixepoch())

-- statement 2 of 5 (setup)
drop table if exists temp_inserted_event

-- statement 3 of 5 (setup)
create temp table temp_inserted_event as
  select last_insert_rowid() as id

-- statement 4 of 5 (returns rows)
select json_group_array(json(affected_row)) as _affectedRows
from (
  select json_object(
    'table_name', 'events',
    'headers', json_array('id', 'name', 'payload', 'tags', 'counts', 'updatedAt'),
    'rows', json_group_array(json_array("events"."id", "events"."name", "events"."payload", "events"."tags", "events"."counts", "events"."updatedAt"))
  ) as affected_row
  from "events"
  join temp_inserted_event temp_table on "events".rowid = temp_table.id
)

-- statement 5 of 5 (returns rows)
select
  coalesce(json_group_array(
    json_object(
      'name', t."name",
      'payload', t."payload",
      'tags', t."tags",
      'counts', t."counts"
    )
  ), json('[]')) as event
from events t
join temp_inserted_event temp_table on t.rowid = temp_table.id

