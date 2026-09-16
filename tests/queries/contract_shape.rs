use crate::helpers::test_database::TestDatabase;
use crate::helpers::TestError;
use serde_json::json;

#[tokio::test]
async fn test_direct_query_nested_tagged_value_uses_tagged_object_shape() -> Result<(), TestError> {
    let schema = r#"
@syncable(false)

type TileFormat
   = Png
   | Webp

record Game {
    id Id.Int @id
    gameMaps @link(GameMap.gameId)
    @public
}

record GameMap {
    id Id.Int @id
    gameId Game.id
    tileFormat TileFormat?

    game @link(gameId, Game.id)
    @public
}
"#;

    let db = TestDatabase::new(schema).await?;
    db.execute_raw("insert into games (id) values (1)").await?;
    db.execute_raw("insert into gameMaps (id, gameId, tileFormat) values (1, 1, 'Png')")
        .await?;

    let query = r#"
        query GetGameWorld {
            game {
                id
                gameMaps {
                    id
                    tileFormat
                }
            }
        }
    "#;

    let rows = db.execute_query(query).await?;
    let results = db.parse_query_results(rows).await?;

    let games = results.get("game").expect("Results should contain 'game'");
    let maps = games[0]
        .get("gameMaps")
        .and_then(|value| value.as_array())
        .expect("Game should include gameMaps array");

    assert_eq!(
        maps[0]
            .get("tileFormat")
            .expect("Map should include tileFormat"),
        &json!({ "_type": "Png" })
    );

    Ok(())
}

#[tokio::test]
async fn test_direct_query_many_to_one_with_nested_collections_uses_object_shape(
) -> Result<(), TestError> {
    let schema = r#"
@syncable(false)

record Game {
    id Id.Int @id
    rulebookVersionId RulebookVersion.id

    rulebookVersion @link(rulebookVersionId, RulebookVersion.id)
    @public
}

record RulebookVersion {
    id Id.Int @id
    versionTag String

    documents @link(RulebookVersionDocument.rulebookVersionId)
    @public
}

record RulebookDocument {
    id Id.Int @id
    contentHash String
    content String

    versions @link(RulebookVersionDocument.rulebookDocumentId)
    @public
}

record RulebookVersionDocument {
    id Id.Int @id
    rulebookVersionId RulebookVersion.id
    rulebookDocumentId RulebookDocument.id
    path String
    orderIndex Int

    rulebookVersion @link(rulebookVersionId, RulebookVersion.id)
    rulebookDocument @link(rulebookDocumentId, RulebookDocument.id)
    @public
}
"#;

    let db = TestDatabase::new(schema).await?;
    db.execute_raw("insert into rulebookVersions (id, versionTag) values (7, 'v1')")
        .await?;
    db.execute_raw(
        "insert into rulebookDocuments (id, contentHash, content) values (9, 'abc', 'body')",
    )
    .await?;
    db.execute_raw(
        "insert into rulebookVersionDocuments (id, rulebookVersionId, rulebookDocumentId, path, orderIndex) values (11, 7, 9, 'intro', 0)",
    )
    .await?;
    db.execute_raw("insert into games (id, rulebookVersionId) values (1, 7)")
        .await?;

    let query = r#"
        query Game($id: Game.id) {
            game {
                @where { id == $id }

                id
                rulebookVersion {
                    id
                    versionTag
                    documents {
                        id
                        path
                        orderIndex
                        rulebookDocument {
                            id
                            contentHash
                            content
                        }
                    }
                }
            }
        }
    "#;

    let rows = db
        .execute_query_with_params(
            query,
            std::collections::HashMap::from([("id".to_string(), libsql::Value::Integer(1))]),
        )
        .await?;
    let results = db.parse_query_results(rows).await?;

    let games = results.get("game").expect("Results should contain 'game'");
    let rulebook_version = games[0]
        .get("rulebookVersion")
        .expect("Game should include rulebookVersion");

    assert!(
        rulebook_version.is_object(),
        "Expected rulebookVersion to be an object, got: {rulebook_version:?}"
    );

    assert_eq!(rulebook_version.get("id"), Some(&json!(7)));
    assert_eq!(rulebook_version.get("versionTag"), Some(&json!("v1")));

    Ok(())
}

#[tokio::test]
async fn test_lore_like_game_queries_preserve_generated_contract_shape() -> Result<(), TestError> {
    let schema = r#"
@syncable(false)

type TileFormat
   = Png
   | Webp

record Game {
    id Id.Int @id
    name String
    rulebookVersionId RulebookVersion.id

    rulebookVersion @link(rulebookVersionId, RulebookVersion.id)
    gameMaps @link(GameMap.gameId)
    @public
}

record GameMap {
    id Id.Int @id
    gameId Game.id
    mapId String
    name String
    tileFormat TileFormat?

    game @link(gameId, Game.id)
    @public
}

record RulebookVersion {
    id Id.Int @id
    versionTag String

    documents @link(RulebookVersionDocument.rulebookVersionId)
    rules @link(RulebookVersionRules.rulebookVersionId)
    @public
}

record RulebookDocument {
    id Id.Int @id
    contentHash String
    content String

    versions @link(RulebookVersionDocument.rulebookDocumentId)
    @public
}

record RulebookRules {
    id Id.Int @id
    contentHash String
    content String

    versions @link(RulebookVersionRules.rulebookRulesId)
    @public
}

record RulebookVersionDocument {
    id Id.Int @id
    rulebookVersionId RulebookVersion.id
    rulebookDocumentId RulebookDocument.id
    path String
    orderIndex Int

    rulebookVersion @link(rulebookVersionId, RulebookVersion.id)
    rulebookDocument @link(rulebookDocumentId, RulebookDocument.id)
    @public
}

record RulebookVersionRules {
    id Id.Int @id
    rulebookVersionId RulebookVersion.id
    rulebookRulesId RulebookRules.id
    path String
    orderIndex Int

    rulebookVersion @link(rulebookVersionId, RulebookVersion.id)
    rulebookRules @link(rulebookRulesId, RulebookRules.id)
    @public
}
"#;

    let db = TestDatabase::new(schema).await?;
    db.execute_raw("insert into rulebookVersions (id, versionTag) values (7, 'v1')")
        .await?;
    db.execute_raw("insert into rulebookDocuments (id, contentHash, content) values (9, 'doc-hash', 'doc-body')")
        .await?;
    db.execute_raw("insert into rulebookRules (id, contentHash, content) values (10, 'rules-hash', 'rules-body')")
        .await?;
    db.execute_raw(
        "insert into rulebookVersionDocuments (id, rulebookVersionId, rulebookDocumentId, path, orderIndex) values (11, 7, 9, 'intro', 0)",
    )
    .await?;
    db.execute_raw(
        "insert into rulebookVersionRules (id, rulebookVersionId, rulebookRulesId, path, orderIndex) values (12, 7, 10, 'combat', 1)",
    )
    .await?;
    db.execute_raw("insert into games (id, name, rulebookVersionId) values (1, 'Lore', 7)")
        .await?;
    db.execute_raw(
        "insert into gameMaps (id, gameId, mapId, name, tileFormat) values (21, 1, 'map-1', 'World', 'Png')",
    )
    .await?;

    let game_query = r#"
        query Game($id: Game.id) {
            game {
                @where { id == $id }

                id
                name
                rulebookVersion {
                    id
                    versionTag
                    documents {
                        id
                        path
                        orderIndex
                        rulebookDocument {
                            id
                            contentHash
                            content
                        }
                    }
                    rules {
                        id
                        path
                        orderIndex
                        rulebookRules {
                            id
                            contentHash
                            content
                        }
                    }
                }
            }
        }
    "#;

    let get_game_world_query = r#"
        query GetGameWorld($id: Game.id) {
            game {
                @where { id == $id }

                id
                name
                rulebookVersionId
                gameMaps {
                    id
                    mapId
                    name
                    tileFormat
                }
            }
        }
    "#;

    let game_rows = db
        .execute_query_with_params(
            game_query,
            std::collections::HashMap::from([("id".to_string(), libsql::Value::Integer(1))]),
        )
        .await?;
    let game_results = db.parse_query_results(game_rows).await?;

    let game = &game_results
        .get("game")
        .expect("Results should contain 'game'")[0];
    let rulebook_version = game
        .get("rulebookVersion")
        .expect("Game should include rulebookVersion");

    assert!(
        rulebook_version.is_object(),
        "Expected rulebookVersion to be an object, got: {rulebook_version:?}"
    );
    assert_eq!(rulebook_version.get("id"), Some(&json!(7)));

    let documents = rulebook_version
        .get("documents")
        .and_then(|value| value.as_array())
        .expect("rulebookVersion should include documents array");
    assert_eq!(documents[0].get("path"), Some(&json!("intro")));

    let rules = rulebook_version
        .get("rules")
        .and_then(|value| value.as_array())
        .expect("rulebookVersion should include rules array");
    assert_eq!(rules[0].get("path"), Some(&json!("combat")));

    let world_rows = db
        .execute_query_with_params(
            get_game_world_query,
            std::collections::HashMap::from([("id".to_string(), libsql::Value::Integer(1))]),
        )
        .await?;
    let world_results = db.parse_query_results(world_rows).await?;

    let world_game = &world_results
        .get("game")
        .expect("Results should contain 'game'")[0];
    let maps = world_game
        .get("gameMaps")
        .and_then(|value| value.as_array())
        .expect("Game should include gameMaps array");

    assert_eq!(maps[0].get("mapId"), Some(&json!("map-1")));
    assert_eq!(
        maps[0]
            .get("tileFormat")
            .expect("Map should include tileFormat"),
        &json!({ "_type": "Png" })
    );

    Ok(())
}
