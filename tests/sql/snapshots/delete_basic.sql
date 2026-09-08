-- query: DeleteUser
-- statement 1 of 1 (returns rows)
delete from users
where
 "users"."id" = $id
 returning json_object() as "user"
