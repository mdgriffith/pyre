-- query: UpdateEvent
-- statement 1 of 1 (returns rows)
update events
set payload = jsonb($payload), updatedAt = unixepoch()
where
 "events"."id" = $id
 returning json_object('payload', json("payload")) as "event", json_array(json_object('table_name', 'events', 'headers', json_array('id', 'name', 'payload', 'tags', 'counts', 'updatedAt'), 'rows', json_array(json_array("id", "name", "payload", "tags", "counts", "updatedAt")))) as _affectedRows

