use crate::helpers::schema;
use crate::helpers::test_database::TestDatabase;
use crate::helpers::TestError;
use serde_json::json;

#[tokio::test]
async fn test_query_with_one_to_many() -> Result<(), TestError> {
    let db = TestDatabase::new(&schema::full_schema()).await?;
    db.seed_standard_data().await?;

    let query = r#"
        query GetUserWithPosts {
            user {
                id
                name
                posts {
                    id
                    title
                    content
                }
            }
        }
    "#;

    let rows = db.execute_query(query).await?;
    let results = db.parse_query_results(rows).await?;

    assert!(
        results.contains_key("user"),
        "Results should contain 'user' field"
    );
    let users = results.get("user").unwrap();

    // Find user with posts
    let user_with_posts = users.iter().find(|u| {
        u.get("posts")
            .and_then(|p| p.as_array())
            .map(|arr| !arr.is_empty())
            .unwrap_or(false)
    });

    assert!(
        user_with_posts.is_some(),
        "Should have at least one user with posts"
    );

    let posts = user_with_posts
        .unwrap()
        .get("posts")
        .and_then(|p| p.as_array())
        .unwrap();
    assert!(posts.len() >= 2, "User should have at least 2 posts");

    Ok(())
}

#[tokio::test]
async fn test_query_with_many_to_one() -> Result<(), TestError> {
    let db = TestDatabase::new(&schema::full_schema()).await?;
    db.seed_standard_data().await?;

    let query = r#"
        query GetPostsWithAuthor {
            post {
                id
                title
                author {
                    id
                    name
                }
            }
        }
    "#;

    let rows = db.execute_query(query).await?;
    let results = db.parse_query_results(rows).await?;

    assert!(
        results.contains_key("post"),
        "Results should contain 'post' field"
    );
    let posts = results.get("post").unwrap();
    assert!(posts.len() >= 2, "Should have at least 2 posts");

    // Verify all posts have author
    for post in posts {
        assert!(
            post.get("author").is_some(),
            "Post should have author field"
        );
        let author = post.get("author").unwrap();
        assert!(author.get("id").is_some(), "Author should have id");
        assert!(author.get("name").is_some(), "Author should have name");
    }

    Ok(())
}

#[tokio::test]
async fn test_nested_json_column_is_returned_as_json_value() -> Result<(), TestError> {
    let schema = r#"
record Game {
    id Int @id
    entities @link(id, GameEntity.gameId)
    @public
}

record GameEntity {
    id Int @id
    gameId Int
    attrs Json
    game @link(gameId, Game.id)
    @public
}
"#;

    let db = TestDatabase::new(schema).await?;
    db.execute_raw("insert into games (id) values (1)").await?;
    db.execute_raw(
        r#"insert into gameEntities (id, gameId, attrs)
           values (1, 1, json('{"name":{"kind":"text","value":"Alyra"}}'))"#,
    )
    .await?;

    let query = r#"
        query GetGame {
            game {
                id
                entities {
                    id
                    attrs
                }
            }
        }
    "#;

    let rows = db.execute_query(query).await?;
    let results = db.parse_query_results(rows).await?;

    let games = results.get("game").expect("Results should contain 'game'");
    let entities = games[0]
        .get("entities")
        .and_then(|value| value.as_array())
        .expect("Game should include entities array");
    let attrs = entities[0]
        .get("attrs")
        .expect("Entity should include attrs");

    assert_eq!(
        attrs,
        &json!({
            "name": {
                "kind": "text",
                "value": "Alyra"
            }
        })
    );

    Ok(())
}

#[tokio::test]
async fn test_duplicate_nested_link_names_use_distinct_ctes() -> Result<(), TestError> {
    let schema = r#"
record Outfit {
    id Int @id
    garments @link(id, OutfitGarment.outfitId)
    previews @link(id, Preview.outfitId)
    @public
}

record OutfitGarment {
    id Int @id
    outfitId Int
    garmentId Int
    garment @link(garmentId, Garment.id)
    @public
}

record Garment {
    id Int @id
    images @link(id, GarmentImage.garmentId)
    @public
}

record GarmentImage {
    id Int @id
    garmentId Int
    imageId Int
    image @link(imageId, Image.id)
    @public
}

record Preview {
    id Int @id
    outfitId Int
    imageId Int
    image @link(imageId, Image.id)
    @public
}

record Image {
    id Int @id
    path String
    @public
}
"#;

    let db = TestDatabase::new(schema).await?;
    db.execute_raw("insert into outfits (id) values (1)")
        .await?;
    db.execute_raw("insert into garments (id) values (10)")
        .await?;
    db.execute_raw("insert into images (id, path) values (20, 'garment.jpg'), (21, 'preview.jpg')")
        .await?;
    db.execute_raw("insert into outfitGarments (id, outfitId, garmentId) values (30, 1, 10)")
        .await?;
    db.execute_raw("insert into garmentImages (id, garmentId, imageId) values (40, 10, 20)")
        .await?;
    db.execute_raw("insert into previews (id, outfitId, imageId) values (50, 1, 21)")
        .await?;

    let query = r#"
query ReproduceDuplicateImageCte {
    outfit {
        id
        garments {
            garment {
                id
                images {
                    image {
                        path
                    }
                }
            }
        }
        previews {
            id
            image {
                path
            }
        }
    }
}
"#;

    let rows = db.execute_query(query).await?;
    let results = db.parse_query_results(rows).await?;
    let outfit = &results["outfit"][0];

    assert_eq!(
        outfit["garments"][0]["garment"]["images"][0]["image"]["path"],
        json!("garment.jpg")
    );
    assert_eq!(outfit["previews"][0]["image"]["path"], json!("preview.jpg"));

    Ok(())
}
