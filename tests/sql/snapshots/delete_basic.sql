-- query: DeleteUser
-- statement 1 of 1 (returns rows)
delete from users
where
 "users"."id" = $id
 returning json_object() as "user", json_array(json_object('table_name', 'users', 'headers', json_array('id', 'name', 'status', 'status__reason', 'updatedAt'), 'rows', json_array(json_array("id", "name", "status", "status__reason", "updatedAt")))) as _affectedRows

