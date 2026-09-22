-- query: DeleteDocument
-- statement 1 of 1 (returns rows)
delete from documents
where
 ("documents"."id" = $id and ("documents"."ownerId" = $session_userId and $session_role = 'admin'))
 returning json_object() as "document", json_array(json_insert(json_object('table_name', 'documents', 'primary_key', 'id', 'headers', json_array('id', 'title', 'content', 'ownerId', 'visibility', 'updatedAt'), 'rows', json_array(json_array("id", "title", "content", "ownerId", "visibility", "updatedAt"))), '$.headers[#]', '_pyre_removed', '$.rows[0][#]', json('true'))) as _affectedRows
