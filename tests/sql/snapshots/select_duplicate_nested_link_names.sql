-- query: ReproduceDuplicateImageCte
-- statement 1 of 1 (returns rows)
with temp_selected_outfit as (
    select id
    from outfits

), temp_selected_outfit__garments as (
    select t.outfitId, t.garmentId
    from outfitGarments t
    where t.outfitId in (select id from temp_selected_outfit)
), temp_selected_outfit__garments__garment as (
    select t.id, t.id
    from garments t
    where t.id in (select garmentId from temp_selected_outfit__garments)
), temp_selected_outfit__garments__garment__images as (
    select t.garmentId, t.imageId
    from garmentImages t
    where t.garmentId in (select id from temp_selected_outfit__garments__garment)
), temp_selected_outfit__garments__garment__images__image as (
    select
      t.id,
      jsonb_object(
        'path', t.path
      ) as image
    from images t
    where t.id in (select imageId from temp_selected_outfit__garments__garment__images)
), json__outfit__garments__garment__images as (
    select
      temp_selected_outfit__garments__garment__images.garmentId,
      jsonb_group_array(jsonb_object(
        'image', temp__image.image
      )) as images
    from temp_selected_outfit__garments__garment__images
      left join temp_selected_outfit__garments__garment__images__image temp__image on temp__image.id = temp_selected_outfit__garments__garment__images.imageId
    group by temp_selected_outfit__garments__garment__images.garmentId
    order by temp_selected_outfit__garments__garment__images.garmentId
), json__outfit__garments__garment as (
    select
      temp_selected_outfit__garments__garment.id,
      jsonb_object(
        'id', temp_selected_outfit__garments__garment.id,
        'images', coalesce(temp__images.images, jsonb('[]'))
      ) as garment
    from temp_selected_outfit__garments__garment
      left join json__outfit__garments__garment__images temp__images on temp__images.garmentId = temp_selected_outfit__garments__garment.id
), json__outfit__garments as (
    select
      temp_selected_outfit__garments.outfitId,
      jsonb_group_array(jsonb_object(
        'garment', temp__garment.garment
      )) as garments
    from temp_selected_outfit__garments
      left join json__outfit__garments__garment temp__garment on temp__garment.id = temp_selected_outfit__garments.garmentId
    group by temp_selected_outfit__garments.outfitId
    order by temp_selected_outfit__garments.outfitId
), temp_selected_outfit__previews as (
    select t.outfitId, t.id, t.imageId
    from previews t
    where t.outfitId in (select id from temp_selected_outfit)
), temp_selected_outfit__previews__image as (
    select
      t.id,
      jsonb_object(
        'path', t.path
      ) as image
    from images t
    where t.id in (select imageId from temp_selected_outfit__previews)
), json__outfit__previews as (
    select
      temp_selected_outfit__previews.outfitId,
      jsonb_group_array(jsonb_object(
        'id', temp_selected_outfit__previews.id,
        'image', temp__image.image
      )) as previews
    from temp_selected_outfit__previews
      left join temp_selected_outfit__previews__image temp__image on temp__image.id = temp_selected_outfit__previews.imageId
    group by temp_selected_outfit__previews.outfitId
    order by temp_selected_outfit__previews.outfitId
)
select
  coalesce(json_group_array(
    json_object(
      'id', temp_selected_outfit.id,
      'garments', coalesce(temp__garments.garments, jsonb('[]')),
      'previews', coalesce(temp__previews.previews, jsonb('[]'))
    )
  ), json('[]')) as outfit
from temp_selected_outfit
  left join json__outfit__garments temp__garments on temp__garments.outfitId = temp_selected_outfit.id
  left join json__outfit__previews temp__previews on temp__previews.outfitId = temp_selected_outfit.id

