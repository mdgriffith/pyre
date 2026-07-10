-- query: DeleteDocument
-- statement 1 of 5 (setup)
drop table if exists temp_deleted_documents

-- statement 2 of 5 (setup)
create temp table temp_deleted_documents as select * from "documents" where
 ("documents"."id" = $id and ("documents"."ownerId" = $session_userId and $session_role = 'admin'))

-- statement 3 of 5 (setup)
delete from documents
where
 ("documents"."id" = $id and ("documents"."ownerId" = $session_userId and $session_role = 'admin'))

-- statement 4 of 5 (returns rows)
select json_group_array(json(affected_row)) as _affectedRows
from (
  select json_object(
    'table_name', 'documents',
    'headers', json_array('id', 'title', 'content', 'ownerId', 'visibility', 'updatedAt'),
    'rows', json_group_array(json_array(temp_deleted_documents."id", temp_deleted_documents."title", temp_deleted_documents."content", temp_deleted_documents."ownerId", temp_deleted_documents."visibility", temp_deleted_documents."updatedAt"))
  ) as affected_row
  from temp_deleted_documents
)

-- statement 5 of 5 (returns rows)
select
  coalesce(json_group_array(
    json_object(

    )
  ), json('[]')) as document
from temp_deleted_documents

