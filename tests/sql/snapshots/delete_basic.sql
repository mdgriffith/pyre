-- query: DeleteUser
-- statement 1 of 1 (returns rows)
delete from users
where
 "users"."id" = $id
 returning json_object() as "user", json_array(json_insert(json_object('table_name', 'users', 'primary_key', 'id', 'headers', json_array('id', 'name', 'status', 'status__reason', 'updatedAt'), 'rows', json_array(json_array("id", "name", "status", "status__reason", "updatedAt"))), '$.headers[#]', '_pyre_removed', '$.rows[0][#]', json('true'))) as _affectedRows
