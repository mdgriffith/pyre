-- query: RenameUser
-- statement 1 of 2 (returns rows)
select json_array(json_insert(json_object('table_name', 'users', 'primary_key', 'id', 'headers', json_array('id', 'name', 'status', 'status__reason', 'updatedAt'), 'rows', json_array(json_array("id", "name", "status", "status__reason", "updatedAt"))), '$.headers[#]', '_pyre_preimage', '$.rows[0][#]', json('true'))) as _affectedRows from users
where
 "users"."id" = $id

-- statement 2 of 2 (returns rows)
update users
set name = $name, updatedAt = unixepoch()
where
 "users"."id" = $id
 returning json_object('name', "name") as "user", json_array(json_object('table_name', 'users', 'primary_key', 'id', 'headers', json_array('id', 'name', 'status', 'status__reason', 'updatedAt'), 'rows', json_array(json_array("id", "name", "status", "status__reason", "updatedAt")))) as _affectedRows

