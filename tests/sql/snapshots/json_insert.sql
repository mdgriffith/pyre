-- query: CreateEvent
-- statement 1 of 1 (returns rows)
insert into events (name, payload, tags, counts, updatedAt)
values ($name, jsonb($payload), jsonb($tags), jsonb($counts), unixepoch()) returning json_object('name', "name", 'payload', json("payload"), 'tags', json("tags"), 'counts', json("counts")) as "event", json_array(json_object('table_name', 'events', 'headers', json_array('id', 'name', 'payload', 'tags', 'counts', 'updatedAt'), 'rows', json_array(json_array("id", "name", "payload", "tags", "counts", "updatedAt")))) as _affectedRows

