-- query: VisibleWorkspaces
-- statement 1 of 1 (returns rows)
with temp_selected_workspace as (
select id
from workspaces
where
 exists (select 1 from "accounts" as "__pyre_exists_0" where "__pyre_exists_0"."workspaceId" = "workspaces"."id" and ("__pyre_exists_0"."state" = 'Failed' and "__pyre_exists_0"."state__code" is not 'blocked'))

)

select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_workspace.id
    )
  ), json('[]')) as workspace
from temp_selected_workspace

