import type { SqlInfo } from './types';

export const sql: SqlInfo[] = [  {
    include: true,
    params: [ "count", "details", "enabled", "id", "release", "role" ],
    sql: `insert into entries (id, release, enabled, count, role, details, updatedAt)
values ($id, $release, $enabled, $count, $role, jsonb($details), unixepoch()) returning json_object('id', "id", 'release', "release", 'enabled', json(case when "enabled" = 1 then 'true' else 'false' end), 'count', "count", 'role',
  json(case
    when entries.role = 'Member' then json_object('_type', 'Member')
    when entries.role = 'Admin' then json_object('_type', 'Admin')
  end), 'details', json("details"), 'updatedAt', "updatedAt") as "entry", "id" as _pyreEditId`
  }
];
export const syncSql: SqlInfo[] = [  {
    include: true,
    params: [ "count", "details", "enabled", "id", "release", "role" ],
    sql: `insert into entries (id, release, enabled, count, role, details, updatedAt)
values ($id, $release, $enabled, $count, $role, jsonb($details), unixepoch()) returning json_object('id', "id", 'release', "release", 'enabled', json(case when "enabled" = 1 then 'true' else 'false' end), 'count', "count", 'role',
  json(case
    when entries.role = 'Member' then json_object('_type', 'Member')
    when entries.role = 'Admin' then json_object('_type', 'Admin')
  end), 'details', json("details"), 'updatedAt', "updatedAt") as "entry", json_array(json_object('table_name', 'entries', 'headers', json_array('id', 'release', 'enabled', 'count', 'role', 'details', 'updatedAt'), 'rows', json_array(json_array("id", "release", "enabled", "count", "role", "details", "updatedAt")))) as _affectedRows, "id" as _pyreEditId`
  }
];
