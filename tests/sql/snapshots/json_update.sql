-- query: UpdateEvent
-- statement 1 of 2 (returns rows)
select json_array(json_insert(json_object('table_name', 'events', 'primary_key', 'id', 'headers', json_array('id', 'name', 'payload', 'tags', 'counts', 'updatedAt'), 'rows', json_array(json_array("id", "name", "payload", "tags", "counts", "updatedAt"))), '$.headers[#]', '_pyre_preimage', '$.rows[0][#]', json('true'))) as _affectedRows from events
where
 "events"."id" = $id

-- statement 2 of 2 (returns rows)
update events
set payload = jsonb($payload), updatedAt = unixepoch()
where
 "events"."id" = $id
 returning json_object('payload', json("payload")) as "event", json_array(json_object('table_name', 'events', 'primary_key', 'id', 'headers', json_array('id', 'name', 'payload', 'tags', 'counts', 'updatedAt'), 'rows', json_array(json_array("id", "name", "payload", "tags", "counts", "updatedAt")))) as _affectedRows

