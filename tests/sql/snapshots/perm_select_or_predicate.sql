-- query: GetArticles
-- statement 1 of 1 (returns rows)
with temp_selected_article as (
select id, status, title
from articles
where
 ("articles"."authorId" = $session_userId or "articles"."status" = 'published')

)

select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_article.id,
      'title', temp_selected_article.title,
      'status', temp_selected_article.status
    )
  ), json('[]')) as article
from temp_selected_article

