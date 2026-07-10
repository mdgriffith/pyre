-- query: CreateUser
-- statement 1 of 1 (returns rows)
insert into users (name, status, updatedAt)
values ($name, 'Active', unixepoch()) returning json_object('name', "name", 'status',
  case
    when users.status = 'Active' then json_object('_type', 'Active')
    when users.status = 'Inactive' then json_object('_type', 'Inactive')
    when users.status = 'Special' then
      json_object(
        '_type', 'Special',
        'reason', users.status__reason
      )
  end) as "user", json_array(json_object('table_name', 'users', 'headers', json_array('id', 'name', 'status', 'status__reason', 'updatedAt'), 'rows', json_array(json_array("id", "name", "status", "status__reason", "updatedAt")))) as _affectedRows
