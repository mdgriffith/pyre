-- query: RenameUser
-- statement 1 of 1 (returns rows)
update users
set name = $name, updatedAt = unixepoch()
where
 "users"."id" = $id
 returning json_object('name', "name") as "user", json_array(json_object('table_name', 'users', 'headers', json_array('id', 'name', 'status', 'status__reason', 'updatedAt'), 'rows', json_array(json_array("id", "name", "status", "status__reason", "updatedAt")))) as _affectedRows

