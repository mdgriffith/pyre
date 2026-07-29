-- query: VisibleJobs
-- statement 1 of 1 (returns rows)
with temp_selected_job as (
select id
from jobs
where
 ("jobs"."state" = 'Failed' and "jobs"."state__reason" = 'ProviderRejected' and "jobs"."state__reason__code" is not 'blocked')

)

select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_job.id
    )
  ), json('[]')) as job
from temp_selected_job

