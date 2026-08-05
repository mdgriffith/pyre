use crate::helpers::schema;
use crate::helpers::test_database::TestDatabase;
use crate::helpers::TestError;

#[tokio::test]
async fn test_simple_query() -> Result<(), TestError> {
    let db = TestDatabase::new(&schema::full_schema()).await?;
    db.seed_standard_data().await?;

    let query = r#"
        query GetUsers {
            user {
                id
                name
            }
        }
    "#;

    let rows = db.execute_query(query).await?;
    let results = db.parse_query_results(rows).await?;

    assert!(
        results.contains_key("user"),
        "Results should contain 'user' field. Available keys: {:?}",
        results.keys().collect::<Vec<_>>()
    );
    let users = results.get("user").unwrap();
    assert!(
        users.len() >= 2,
        "Should have at least 2 users, but got {}. Users: {:#}",
        users.len(),
        serde_json::json!(users)
    );

    Ok(())
}

#[tokio::test]
async fn test_root_alias_is_used_as_response_key() -> Result<(), TestError> {
    let db = TestDatabase::new(&schema::full_schema()).await?;
    db.seed_standard_data().await?;

    let rows = db
        .execute_query(
            r#"
query GetPeople {
    people: user {
        id
        name
    }
}
"#,
        )
        .await?;
    let results = db.parse_query_results(rows).await?;

    assert!(results.contains_key("people"));
    assert!(!results.contains_key("user"));
    Ok(())
}

#[tokio::test]
async fn test_query_with_fields() -> Result<(), TestError> {
    let db = TestDatabase::new(&schema::full_schema()).await?;
    db.seed_standard_data().await?;

    let query = r#"
        query GetUsers {
            user {
                id
                name
                status
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

    // Verify all users have the requested fields
    for user in users {
        assert!(user.get("id").is_some(), "User should have id field");
        assert!(user.get("name").is_some(), "User should have name field");
        assert!(
            user.get("status").is_some(),
            "User should have status field"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_wildcard_selects_scalars_only() -> Result<(), TestError> {
    let db = TestDatabase::new(&schema::full_schema()).await?;
    db.seed_standard_data().await?;

    let query = r#"
        query GetUsers {
            user {
                *
            }
        }
    "#;

    let rows = db.execute_query(query).await?;
    let results = db.parse_query_results(rows).await?;

    let users = results.get("user").expect("Results should contain 'user'");
    assert!(!users.is_empty(), "Should return at least one user");

    for user in users {
        assert!(user.get("id").is_some(), "User should have id field");
        assert!(user.get("name").is_some(), "User should have name field");
        assert!(
            user.get("status").is_some(),
            "User should have status field"
        );
        assert!(
            user.get("posts").is_none(),
            "Wildcard should not include posts"
        );
        assert!(
            user.get("accounts").is_none(),
            "Wildcard should not include accounts"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_wildcard_with_explicit_field() -> Result<(), TestError> {
    let db = TestDatabase::new(&schema::full_schema()).await?;
    db.seed_standard_data().await?;

    let query = r#"
        query GetUsers {
            user {
                *
                name
            }
        }
    "#;

    let rows = db.execute_query(query).await?;
    let results = db.parse_query_results(rows).await?;

    let users = results.get("user").expect("Results should contain 'user'");
    assert!(!users.is_empty(), "Should return at least one user");

    for user in users {
        assert!(user.get("name").is_some(), "User should have name field");
        assert!(
            user.get("posts").is_none(),
            "Wildcard should not include posts"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_wildcard_multiple_times() -> Result<(), TestError> {
    let db = TestDatabase::new(&schema::full_schema()).await?;
    db.seed_standard_data().await?;

    let query = r#"
        query GetUsers {
            user {
                *
                *
            }
        }
    "#;

    let rows = db.execute_query(query).await?;
    let results = db.parse_query_results(rows).await?;

    let users = results.get("user").expect("Results should contain 'user'");
    assert!(!users.is_empty(), "Should return at least one user");

    for user in users {
        assert!(user.get("id").is_some(), "User should have id field");
        assert!(user.get("name").is_some(), "User should have name field");
        assert!(
            user.get("status").is_some(),
            "User should have status field"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_wildcard_nested_relation() -> Result<(), TestError> {
    let db = TestDatabase::new(&schema::full_schema()).await?;
    db.seed_standard_data().await?;

    let query = r#"
        query GetPosts {
            post {
                id
                author {
                    *
                }
            }
        }
    "#;

    let rows = db.execute_query(query).await?;
    let results = db.parse_query_results(rows).await?;

    let posts = results.get("post").expect("Results should contain 'post'");
    assert!(!posts.is_empty(), "Should return at least one post");

    for post in posts {
        let author = post.get("author").expect("Post should have author");
        assert!(author.get("id").is_some(), "Author should have id");
        assert!(author.get("name").is_some(), "Author should have name");
        assert!(author.get("status").is_some(), "Author should have status");
        assert!(
            author.get("posts").is_none(),
            "Wildcard should not include posts"
        );
        assert!(
            author.get("accounts").is_none(),
            "Wildcard should not include accounts"
        );
    }

    Ok(())
}
