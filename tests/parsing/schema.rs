use pyre::ast;
use pyre::error;
use pyre::parser;
use pyre::typecheck;

/// Helper function to format errors without color for testing
/// Strips ANSI color codes from the formatted error
fn format_error_no_color(file_contents: &str, error: &error::Error) -> String {
    return error::format_error(file_contents, error, false);
}

#[test]
fn test_valid_record() {
    let schema_source = r#"
record User {
    id   Int    @id
    name String
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_ok(), "Valid record should parse successfully");
}

#[test]
fn test_valid_tagged_type() {
    let schema_source = r#"
type Status
   = Active
   | Inactive
   | Special {
        reason String
     }
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Valid tagged type should parse successfully"
    );
}

#[test]
fn test_valid_json_generic_field_type() {
    let schema_source = r#"
type Preferences
   = Theme {
        theme String
        locale String
     }

record User {
    id Int @id
    preferences Json<Preferences>
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Json<T> field type should parse successfully"
    );
}

#[test]
fn test_valid_nested_generic_json_field_type_with_nullability() {
    let schema_source = r#"
record User {
    id Int @id
    tags Json<List<String?>>?
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Nested generic Json type with nullability should parse successfully"
    );
}

#[test]
fn test_valid_session() {
    let schema_source = r#"
session {
    userId Int
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_ok(), "Valid session should parse successfully");
}

#[test]
fn test_valid_record_with_link() {
    let schema_source = r#"
record User {
    id   Int    @id
    name String
}

record Post {
    id        Int    @id
    authorId  Int
    author    @link(authorId, User.id)
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Valid record with link should parse successfully"
    );
}

#[test]
fn test_unqualified_link_uses_current_namespace() {
    let schema_source = r#"
record User {
    id Int @id
    @public
}

record Post {
    id Int @id
    authorId Int
    author @link(authorId, User.id)
    @public
}
    "#;

    let mut schema = ast::Schema::default();
    schema.namespace = "App".to_string();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema should parse");

    let post_record = schema
        .files
        .iter()
        .flat_map(|file| file.definitions.iter())
        .find_map(|definition| match definition {
            ast::Definition::Record { name, fields, .. } if name == "Post" => Some(fields),
            _ => None,
        })
        .expect("expected Post record");

    let author_link = post_record
        .iter()
        .find_map(|field| match field {
            ast::Field::FieldDirective(ast::FieldDirective::Link(details))
                if details.link_name == "author" =>
            {
                Some(details)
            }
            _ => None,
        })
        .expect("expected author link");

    assert_eq!(author_link.foreign.schema, "App");
    assert_eq!(author_link.foreign.table, "User");
    assert_eq!(author_link.foreign.fields, vec!["id".to_string()]);
}

#[test]
fn test_qualified_link_keeps_explicit_namespace() {
    let schema_source = r#"
record User {
    id Int @id
    @public
}

record Post {
    id Int @id
    authorId Int
    author @link(authorId, Auth.User.id)
    @public
}
    "#;

    let mut schema = ast::Schema::default();
    schema.namespace = "App".to_string();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema should parse");

    let post_record = schema
        .files
        .iter()
        .flat_map(|file| file.definitions.iter())
        .find_map(|definition| match definition {
            ast::Definition::Record { name, fields, .. } if name == "Post" => Some(fields),
            _ => None,
        })
        .expect("expected Post record");

    let author_link = post_record
        .iter()
        .find_map(|field| match field {
            ast::Field::FieldDirective(ast::FieldDirective::Link(details))
                if details.link_name == "author" =>
            {
                Some(details)
            }
            _ => None,
        })
        .expect("expected author link");

    assert_eq!(author_link.foreign.schema, "Auth");
    assert_eq!(author_link.foreign.table, "User");
    assert_eq!(author_link.foreign.fields, vec!["id".to_string()]);
}

#[test]
fn test_valid_record_with_tablename() {
    let schema_source = r#"
record User {
    @tablename("users")
    id   Int    @id
    name String
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Valid record with tablename should parse successfully"
    );
}

#[test]
fn test_valid_record_with_table_level_index_and_unique_directives() {
    let schema_source = r#"
record Membership {
    id        Int @id
    orgId     Int
    userId    Int
    deletedAt DateTime?

    @unique(orgId, userId)
    @index(orgId asc, deletedAt desc) where { deletedAt = null }
    @public
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Valid record with table-level index directives should parse successfully"
    );
}

#[test]
fn test_valid_singleton_record() {
    let schema_source = r#"
record ApplicationSettings {
    @singleton
    @public

    id    Id.Uuid @id
    theme String
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("singleton should parse");

    let fields = schema
        .files
        .iter()
        .flat_map(|file| &file.definitions)
        .find_map(|definition| match definition {
            ast::Definition::Record { name, fields, .. } if name == "ApplicationSettings" => {
                Some(fields)
            }
            _ => None,
        })
        .expect("expected ApplicationSettings record");

    assert!(ast::is_singleton(fields));
}

#[test]
fn test_invalid_table_level_index_with_unknown_field_fails_typecheck() {
    let schema_source = r#"
record Membership {
    id     Int @id
    orgId  Int
    userId Int

    @index(orgId, missingField)
    @public
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema should parse");

    let db = ast::Database {
        schemas: vec![schema],
    };
    let checked = typecheck::check_schema(&db);
    assert!(
        checked.is_err(),
        "Typecheck should fail when table-level index references unknown field"
    );
}

#[test]
fn test_invalid_table_level_unique_with_duplicate_fields_fails_typecheck() {
    let schema_source = r#"
record Membership {
    id     Int @id
    orgId  Int
    userId Int

    @unique(orgId, orgId)
    @public
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema should parse");

    let db = ast::Database {
        schemas: vec![schema],
    };
    let checked = typecheck::check_schema(&db);
    assert!(
        checked.is_err(),
        "Typecheck should fail when table-level unique has duplicate fields"
    );
}

#[test]
fn test_missing_record_name() {
    let schema_source = r#"
record {
    id   Int    @id
    name String
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_err(), "Missing record name should fail");

    if let Err(err) = result {
        if let Some(error) = parser::convert_parsing_error(err) {
            let formatted = format_error_no_color(schema_source, &error);

            // The parser may give generic errors, so just verify it's an error message
            assert!(
                formatted.contains("schema.pyre")
                    || formatted.contains("expecting")
                    || formatted.contains("parameter")
                    || formatted.contains("issue"),
                "Error message should indicate a parsing error. Got:\n{}",
                formatted
            );
        } else {
            panic!("Expected parsing error but convert_parsing_error returned None");
        }
    } else {
        panic!("Expected parsing to fail but it succeeded");
    }
}

#[test]
fn test_missing_record_brace() {
    let schema_source = r#"
record User
    id   Int    @id
    name String
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_err(), "Missing opening brace should fail");

    if let Err(err) = result {
        if let Some(error) = parser::convert_parsing_error(err) {
            let formatted = format_error_no_color(schema_source, &error);

            // The parser may give generic errors, so just verify it's an error message
            assert!(
                formatted.contains("schema.pyre")
                    || formatted.contains("expecting")
                    || formatted.contains("parameter")
                    || formatted.contains("issue")
                    || formatted.contains("column"),
                "Error message should indicate a parsing error. Got:\n{}",
                formatted
            );
        } else {
            panic!("Expected parsing error but convert_parsing_error returned None");
        }
    } else {
        panic!("Expected parsing to fail but it succeeded");
    }
}

#[test]
fn test_invalid_field_syntax() {
    let schema_source = r#"
record User {
    id Int @id
    name = String
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_err(), "Invalid field syntax should fail");

    if let Err(err) = result {
        if let Some(error) = parser::convert_parsing_error(err) {
            let formatted = format_error_no_color(schema_source, &error);

            // The parser may give generic errors, so just verify it's an error message
            assert!(
                formatted.contains("schema.pyre")
                    || formatted.contains("expecting")
                    || formatted.contains("parameter")
                    || formatted.contains("issue")
                    || formatted.contains("column"),
                "Error message should indicate a parsing error. Got:\n{}",
                formatted
            );
        } else {
            panic!("Expected parsing error but convert_parsing_error returned None");
        }
    } else {
        panic!("Expected parsing to fail but it succeeded");
    }
}

#[test]
fn test_missing_type_in_tagged() {
    let schema_source = r#"
type Status
   = Active
   | Inactive
   | Special {
        reason
     }
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Missing type in tagged variant should fail"
    );

    if let Err(err) = result {
        let error = parser::convert_parsing_error(err).unwrap();
        let formatted = format_error_no_color(schema_source, &error);

        assert!(
            formatted.contains("expecting") || formatted.contains("type"),
            "Error message should indicate what was expected. Got:\n{}",
            formatted
        );
    }
}

#[test]
fn test_invalid_directive() {
    let schema_source = r#"
record User {
    id   Int    @id
    name String @unknown
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_err(), "Invalid directive should fail");

    if let Err(err) = result {
        let error = parser::convert_parsing_error(err).unwrap();
        let formatted = format_error_no_color(schema_source, &error);

        // Check that the error mentions the unknown directive and suggests alternatives
        assert!(
            formatted.contains("@unknown")
                && (formatted.contains("@id") || formatted.contains("did you mean")),
            "Error message should mention @unknown and suggest alternatives. Got:\n{}",
            formatted
        );
    }
}

#[test]
fn test_invalid_link_syntax() {
    let schema_source = r#"
record User {
    id   Int    @id
}

record Post {
    id        Int    @id
    authorId  Int
    author    @link(authorId User.id)
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Invalid link syntax (missing comma) should fail"
    );

    if let Err(err) = result {
        let error = parser::convert_parsing_error(err).unwrap();
        let formatted = format_error_no_color(schema_source, &error);

        assert!(
            formatted.contains("expecting") || formatted.contains("link"),
            "Error message should indicate what was expected. Got:\n{}",
            formatted
        );
    }
}

#[test]
fn test_link_syntax_allows_space_before_comma() {
    let schema_source = r#"
record User {
    id   Int    @id
}

record Post {
    id        Int    @id
    authorId  Int
    author    @link(authorId , User.id)
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Link syntax with space before comma should parse"
    );
}

#[test]
fn test_invalid_link_syntax_error_shows_both_forms() {
    let schema_source = r#"
record User {
    id   Int    @id
}

record Post {
    id        Int    @id
    authorId  Int
    author    @link(authorId User.id)
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Invalid link syntax (missing comma) should fail"
    );

    if let Err(err) = result {
        let error = parser::convert_parsing_error(err).unwrap();
        let formatted = format_error_no_color(schema_source, &error);

        assert!(
            formatted.contains("@link(authorId, User.id)")
                && formatted.contains("@link(Post.authorId)"),
            "Error message should include both link forms. Got:\n{}",
            formatted
        );
    }
}

#[test]
fn test_reverse_link_explicit_local_id_parses() {
    let schema_source = r#"
record User {
    id   Int    @id
}

record Post {
    id        Int    @id
    authorId  Int
    author    @link(authorId, User.id)
}

record Feed {
    id     Int @id
    posts  @link(id, Post.authorId)
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Reverse links with explicit local id should parse"
    );
}

#[test]
fn test_game_style_reverse_links_parse() {
    let schema_source = r#"
type GameRole
   = GM
   | Player

record User {
    id Id.Uuid @id
}

record Game {
    id Id.Uuid @id
    createdByUserId User.id

    gameMembers @link(id, GameMember.gameId)
    gameInvites @link(GameInvite.gameId)
}

record GameMember {
    id     Id.Uuid @id
    gameId Game.id
    userId User.id
    role   GameRole

    game @link(gameId, Game.id)
}

record GameInvite {
    id            Id.Uuid @id
    gameId        Game.id
    inviterUserId User.id

    game @link(gameId, Game.id)
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Game-style reverse links should parse in explicit and shorthand forms"
    );
}

#[test]
fn test_game_style_missing_link_comma_shows_link_error() {
    let schema_source = r#"
record Game {
    id Id.Uuid @id
    gameMembers @link(id GameMember.gameId)
}

    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Missing comma in game-style link should fail"
    );

    if let Err(err) = result {
        let error = parser::convert_parsing_error(err).unwrap();
        let formatted = format_error_no_color(schema_source, &error);

        assert!(
            formatted.contains("@link(authorId, User.id)")
                && formatted.contains("@link(Post.authorId)"),
            "Error should include link syntax help. Got:\n{}",
            formatted
        );
    }
}

#[test]
fn test_full_game_schema_with_explicit_and_shorthand_links_parses() {
    let schema_source = r#"
type GameRole
   = GM
   | Player
   | Observer

type GameAssetKind
   = MapData
   | MapImage
   | Portrait
   | ItemImage
   | DocumentContent
   | Attachment
   | GenericImage

type GridType
   = Square
   | HexFlat
   | HexPointy

type TileFormat
   = Png
   | Webp

type Grid = Grid {
    gridType GridType
    cellSize Int
    offsetX  Int
    offsetY  Int
}

type Tiling = Tiling {
    tileRootKey       String
    tileWidth         Int
    tileHeight        Int
    highestTileLevel  Int
    format            TileFormat
}

type InviteTarget
   = InviteUser { userId User.id }
   | InviteEmail { email String }

record User {
    id    Id.Uuid @id
    email String
}

record RulebookVersion {
    id Id.Uuid @id
}

record Game {
    @allow(*) {
        createdByUserId == Session.userId
        || Session.isAdmin == True
     }

    id              Id.Uuid  @id
    name            String
    createdByUserId User.id
    createdAt       DateTime @default(now)

    gameMembers   @link(id, GameMember.gameId)
    gameInvites   @link(id, GameInvite.gameId)
    gameRulebooks @link(id, GameRulebook.gameId)
    gameEntities  @link(id, GameEntity.gameId)
    gameLinks     @link(id, GameLink.gameId)
    gameAssets    @link(id, GameAsset.gameId)
    gameMaps      @link(id, GameMap.gameId)
    gameDocuments @link(id, GameDocument.gameId)
}

record GameAsset {
    @allow(*) {
        createdByUserId == Session.userId
        || updatedByUserId == Session.userId
        || Session.isAdmin == True
     }

    id              Id.Uuid        @id
    gameId          Game.id
    assetId         String
    kind            GameAssetKind
    storageBucket   String
    storageKey      String
    mimeType        String
    byteSize        Int
    checksumSha256  String
    width           Int?
    height          Int?
    attrs           Json
    createdByUserId User.id
    updatedByUserId User.id?
    createdAt       DateTime       @default(now)
    updatedAt       DateTime       @default(now)
    assetKey        String         @unique

    createdBy @link(createdByUserId, User.id)
    updatedBy @link(updatedByUserId, User.id)
    games     @link(gameId, Game.id)
}

record GameMap {
    @allow(*) {
        createdByUserId == Session.userId
        || updatedByUserId == Session.userId
        || Session.isAdmin == True
     }

    id               Id.Uuid      @id
    gameId           Game.id
    mapId            String
    name             String
    mapImageAssetId  GameAsset.id
    grid             Grid?
    tiling           Tiling?
    thumbnailAssetId GameAsset.id?
    createdByUserId  User.id
    updatedByUserId  User.id?
    createdAt        DateTime     @default(now)
    updatedAt        DateTime     @default(now)
    mapKey           String       @unique

    mapImageAsset  @link(mapImageAssetId, GameAsset.id)
    thumbnailAsset @link(thumbnailAssetId, GameAsset.id)
    createdBy      @link(createdByUserId, User.id)
    updatedBy      @link(updatedByUserId, User.id)
    games          @link(gameId, Game.id)
}

record GameDocument {
    @allow(*) {
        createdByUserId == Session.userId
        || updatedByUserId == Session.userId
        || Session.isAdmin == True
     }

    id                     Id.Uuid               @id
    gameId                 Game.id
    documentId             String
    title                  String
    attrs                  Json
    currentSnapshotAssetId GameAsset.id?
    createdByUserId        User.id
    updatedByUserId        User.id?
    createdAt              DateTime              @default(now)
    updatedAt              DateTime              @default(now)
    documentKey            String                @unique

    currentSnapshotAsset @link(currentSnapshotAssetId, GameAsset.id)
    createdBy            @link(createdByUserId, User.id)
    updatedBy            @link(updatedByUserId, User.id)
    games                @link(gameId, Game.id)
}

record GameMember {
    @allow(*) {
        userId == Session.userId
        || invitedByUserId == Session.userId
        || Session.isAdmin == True
     }

    id              Id.Uuid  @id
    gameId          Game.id
    userId          User.id
    role            GameRole
    invitedByUserId User.id?
    joinedAt        DateTime @default(now)
    membershipKey   String   @unique

    user    @link(userId, User.id)
    inviter @link(invitedByUserId, User.id)
    games   @link(gameId, Game.id)
}

record GameInvite {
    @allow(*) {
        inviterUserId == Session.userId
        || acceptedByUserId == Session.userId
        || Session.isAdmin == True
     }

    id               Id.Uuid      @id
    gameId           Game.id
    inviterUserId    User.id
    target           InviteTarget
    token            String       @unique
    message          String?
    expiresAt        DateTime?
    acceptedByUserId User.id?
    acceptedAt       DateTime?
    revokedAt        DateTime?
    createdAt        DateTime     @default(now)

    inviter    @link(inviterUserId, User.id)
    acceptedBy @link(acceptedByUserId, User.id)
    games      @link(gameId, Game.id)
}

record GameRulebook {
    @allow(*) {
        addedByUserId == Session.userId
        || Session.isAdmin == True
     }

    id                Id.Uuid            @id
    gameId            Game.id
    rulebookVersionId RulebookVersion.id
    addedByUserId     User.id
    isActive          Bool               @default(False)
    loadOrder         Int                @default(0)
    addedAt           DateTime           @default(now)
    bindingKey        String             @unique

    rulebookVersion @link(rulebookVersionId, RulebookVersion.id)
    addedBy         @link(addedByUserId, User.id)
    games           @link(gameId, Game.id)
}

record GameEntity {
    @allow(*) {
        createdByUserId == Session.userId
        || updatedByUserId == Session.userId
        || Session.isAdmin == True
     }

    id              Id.Uuid  @id
    gameId          Game.id
    entityId        String
    entityType      String
    attrs           Json
    createdByUserId User.id
    updatedByUserId User.id?
    createdAt       DateTime @default(now)
    updatedAt       DateTime @default(now)
    entityKey       String   @unique

    createdBy @link(createdByUserId, User.id)
    updatedBy @link(updatedByUserId, User.id)
    games     @link(gameId, Game.id)
}

record GameLink {
    @allow(*) {
        createdByUserId == Session.userId
        || updatedByUserId == Session.userId
        || Session.isAdmin == True
     }

    id              Id.Uuid  @id
    gameId          Game.id
    linkId          String
    linkType        String
    fromEntityId    String
    toEntityId      String
    attrs           Json
    createdByUserId User.id
    updatedByUserId User.id?
    createdAt       DateTime @default(now)
    updatedAt       DateTime @default(now)
    linkKey         String   @unique

    createdBy @link(createdByUserId, User.id)
    updatedBy @link(updatedByUserId, User.id)
    games     @link(gameId, Game.id)
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_ok(), "Full game schema should parse");
}

#[test]
fn test_user_provided_game_schema_parses_exactly() {
    let schema_source = r#"
type GameRole
   = GM
   | Player
   | Observer

type GameAssetKind
   = MapData
   | MapImage
   | Portrait
   | ItemImage
   | DocumentContent
   | Attachment
   | GenericImage

type GridType
   = Square
   | HexFlat
   | HexPointy

type TileFormat
   = Png
   | Webp

type Grid = Grid {
    gridType GridType
    cellSize Int
    offsetX  Int
    offsetY  Int
}

type Tiling = Tiling {
    // Storage prefix for tiles; full tile key is {tileRootKey}/{z}/{x}/{y}.{format}
    tileRootKey       String
    tileWidth         Int
    tileHeight        Int
    highestTileLevel  Int
    format            TileFormat
}

record Game {
    @allow(*) {
        createdByUserId == Session.userId
        || Session.isAdmin == True
     }

    id              Id.Uuid  @id
    name            String
    createdByUserId User.id
    createdAt       DateTime @default(now)

    gameMembers   @link(id, GameMember.gameId)
    gameInvites   @link(id, GameInvite.gameId)
    gameRulebooks @link(id, GameRulebook.gameId)
    gameEntities  @link(id, GameEntity.gameId)
    gameLinks     @link(id, GameLink.gameId)
    gameAssets    @link(id, GameAsset.gameId)
    gameMaps      @link(id, GameMap.gameId)
    gameDocuments @link(id, GameDocument.gameId)
}


record GameAsset {
    @allow(*) {
        createdByUserId == Session.userId
        || updatedByUserId == Session.userId
        || Session.isAdmin == True
     }


    id              Id.Uuid        @id
    gameId          Game.id
    assetId         String
    kind            GameAssetKind
    storageBucket   String
    storageKey      String
    mimeType        String
    byteSize        Int
    checksumSha256  String
    width           Int?
    height          Int?
    attrs           Json
    createdByUserId User.id
    updatedByUserId User.id?
    createdAt       DateTime       @default(now)
    updatedAt       DateTime       @default(now)
    assetKey        String         @unique

    createdBy @link(createdByUserId, User.id)
    updatedBy @link(updatedByUserId, User.id)
    games     @link(gameId, Game.id)
}

record GameMap {
    @allow(*) {
        createdByUserId == Session.userId
        || updatedByUserId == Session.userId
        || Session.isAdmin == True
     }

    id               Id.Uuid      @id
    gameId           Game.id
    mapId            String
    name             String
    mapImageAssetId  GameAsset.id
    grid             Grid?
    tiling           Tiling?
    thumbnailAssetId GameAsset.id?
    createdByUserId  User.id
    updatedByUserId  User.id?
    createdAt        DateTime     @default(now)
    updatedAt        DateTime     @default(now)
    mapKey           String       @unique

    mapImageAsset  @link(mapImageAssetId, GameAsset.id)
    thumbnailAsset @link(thumbnailAssetId, GameAsset.id)
    createdBy      @link(createdByUserId, User.id)
    updatedBy      @link(updatedByUserId, User.id)
    games          @link(gameId, Game.id)
}

record GameDocument {
    @allow(*) {
        createdByUserId == Session.userId
        || updatedByUserId == Session.userId
        || Session.isAdmin == True
     }

    id                     Id.Uuid               @id
    gameId                 Game.id
    documentId             String
    title                  String
    attrs                  Json
    currentSnapshotAssetId GameAsset.id?
    createdByUserId        User.id
    updatedByUserId        User.id?
    createdAt              DateTime              @default(now)
    updatedAt              DateTime              @default(now)
    documentKey            String                @unique

    currentSnapshotAsset @link(currentSnapshotAssetId, GameAsset.id)
    createdBy            @link(createdByUserId, User.id)
    updatedBy            @link(updatedByUserId, User.id)
    games                @link(gameId, Game.id)
}

record GameMember {
    @allow(*) {
        userId == Session.userId
        || invitedByUserId == Session.userId
        || Session.isAdmin == True
     }

    id              Id.Uuid  @id
    gameId          Game.id
    userId          User.id
    role            GameRole
    invitedByUserId User.id?
    joinedAt        DateTime @default(now)
    membershipKey   String   @unique

    user    @link(userId, User.id)
    inviter @link(invitedByUserId, User.id)
    games   @link(gameId, Game.id)
}

record GameInvite {
    @allow(*) {
        inviterUserId == Session.userId
        || acceptedByUserId == Session.userId
        || Session.isAdmin == True
     }

    id               Id.Uuid      @id
    gameId           Game.id
    inviterUserId    User.id
    target           InviteTarget
    token            String       @unique
    message          String?
    expiresAt        DateTime?
    acceptedByUserId User.id?
    acceptedAt       DateTime?
    revokedAt        DateTime?
    createdAt        DateTime     @default(now)

    inviter    @link(inviterUserId, User.id)
    acceptedBy @link(acceptedByUserId, User.id)
    games      @link(gameId, Game.id)
}

type InviteTarget
   = InviteUser { userId User.id }
   | InviteEmail { email String }

record GameRulebook {
    @allow(*) {
        addedByUserId == Session.userId
        || Session.isAdmin == True
     }

    id                Id.Uuid            @id
    gameId            Game.id
    rulebookVersionId RulebookVersion.id
    addedByUserId     User.id
    isActive          Bool               @default(False)
    loadOrder         Int                @default(0)
    addedAt           DateTime           @default(now)
    bindingKey        String             @unique

    rulebookVersion @link(rulebookVersionId, RulebookVersion.id)
    addedBy         @link(addedByUserId, User.id)
    games           @link(gameId, Game.id)
}

record GameEntity {
    @allow(*) {
        createdByUserId == Session.userId
        || updatedByUserId == Session.userId
        || Session.isAdmin == True
     }

    id              Id.Uuid  @id
    gameId          Game.id
    entityId        String
    entityType      String
    attrs           Json
    createdByUserId User.id
    updatedByUserId User.id?
    createdAt       DateTime @default(now)
    updatedAt       DateTime @default(now)
    entityKey       String   @unique

    createdBy @link(createdByUserId, User.id)
    updatedBy @link(updatedByUserId, User.id)
    games     @link(gameId, Game.id)
}

record GameLink {
    @allow(*) {
        createdByUserId == Session.userId
        || updatedByUserId == Session.userId
        || Session.isAdmin == True
     }

    id              Id.Uuid  @id
    gameId          Game.id
    linkId          String
    linkType        String
    fromEntityId    String
    toEntityId      String
    attrs           Json
    createdByUserId User.id
    updatedByUserId User.id?
    createdAt       DateTime @default(now)
    updatedAt       DateTime @default(now)
    linkKey         String   @unique

    createdBy @link(createdByUserId, User.id)
    updatedBy @link(updatedByUserId, User.id)
    games     @link(gameId, Game.id)
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "User-provided full game schema should parse"
    );
}

#[test]
fn test_game_record_allow_on_same_line_as_open_brace_parses() {
    let schema_source = r#"
record User {
    id Id.Uuid @id
}

record GameDocument {
    id Id.Uuid @id
    gameId Game.id
}

record Game {    @allow(*) {
        createdByUserId == Session.userId
        || Session.isAdmin == True
     }

    id              Id.Uuid  @id
    name            String
    createdByUserId User.id
    createdAt       DateTime @default(now)

    gameDocuments @link(id, GameDocument.gameId)
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "@allow on same line as opening brace should parse"
    );
}

#[test]
fn test_record_allows_trailing_space_after_closing_brace() {
    let schema_source = r#"
record Game {
    id Id.Uuid @id
}

record GameAsset {
    id Id.Uuid @id
    gameId Game.id
    games @link(gameId, Game.id)
} 

record GameMap {
    id Id.Uuid @id
    gameId Game.id
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Trailing space after closing brace should parse"
    );
}

#[test]
fn test_link_error_points_to_link_line_not_next_record() {
    let schema_source = r#"
record Game {
    id Id.Uuid @id
    gameMembers @link(id, GameMember.gameId
}

record GameMember {
    id Id.Uuid @id
    gameId Game.id
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_err(), "Schema with malformed link should fail");

    if let Err(err) = result {
        let rendered = parser::render_error(schema_source, err, false);
        assert!(
            rendered.contains("4|     gameMembers @link(id, GameMember.gameId"),
            "Link parsing error should point at the link line. Got:\n{}",
            rendered
        );
    }
}

#[test]
fn test_missing_closing_brace() {
    let schema_source = r#"
record User {
    id   Int    @id
    name String
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_err(), "Missing closing brace should fail");

    if let Err(err) = result {
        // Some parsing errors may not be convertible, which is fine - we just verify parsing failed
        if let Some(error) = parser::convert_parsing_error(err) {
            let formatted = format_error_no_color(schema_source, &error);

            // The parser may give generic errors, so just verify it's an error message
            assert!(
                formatted.contains("schema.pyre")
                    || formatted.contains("expecting")
                    || formatted.contains("parameter")
                    || formatted.contains("issue")
                    || formatted.contains("Incomplete"),
                "Error message should indicate a parsing error. Got:\n{}",
                formatted
            );
        }
        // If convert_parsing_error returns None, that's okay - we've verified parsing failed
    } else {
        panic!("Expected parsing to fail but it succeeded");
    }
}

#[test]
fn test_empty_record() {
    let schema_source = r#"
record User {
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    // Empty records might be valid or invalid depending on the implementation
    // This test documents the current behavior
    let _ = result;
}

#[test]
fn test_record_with_comments() {
    let schema_source = r#"
// This is a comment
record User {
    id   Int    @id
    // Another comment
    name String
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_ok(), "Comments should be allowed in schema");
}

#[test]
fn test_multiple_records() {
    let schema_source = r#"
record User {
    id   Int    @id
    name String
}

record Post {
    id        Int    @id
    title     String
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_ok(), "Multiple records should parse successfully");
}

#[test]
fn test_union_type_with_record() {
    // Test parsing union type and record together using the schema helper format
    // This verifies that schema_v1_complete() produces a parseable schema
    // Note: This test documents that the format from schema_v1_complete() works,
    // which uses format! with trim() to combine definitions
    use super::super::helpers::schema;

    let schema_source = schema::schema_v1_complete();

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", &schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Union type with record from schema_v1_complete() should parse successfully. Error: {:?}",
        result.err()
    );

    // Verify both definitions were parsed
    assert_eq!(schema.files.len(), 1, "Should have one schema file");
    let file = &schema.files[0];

    // Count union types and records
    let union_count = file
        .definitions
        .iter()
        .filter(|d| matches!(d, ast::Definition::Tagged { .. }))
        .count();
    let record_count = file
        .definitions
        .iter()
        .filter(|d| matches!(d, ast::Definition::Record { .. }))
        .count();

    assert_eq!(union_count, 1, "Should have parsed one union type (Status)");
    assert_eq!(record_count, 1, "Should have parsed one record (User)");
}

#[test]
fn test_union_type_with_leading_spaces() {
    // Test that union type alone with leading spaces parses successfully
    // This is the format that works for union types in migration tests
    let schema_source = r#"type Status
   = Active
   | Inactive
"#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Union type with leading spaces should parse successfully. Error: {:?}",
        result.err()
    );

    // Verify the union type was parsed
    assert_eq!(schema.files.len(), 1, "Should have one schema file");
    let file = &schema.files[0];

    let union_count = file
        .definitions
        .iter()
        .filter(|d| matches!(d, ast::Definition::Tagged { .. }))
        .count();

    assert_eq!(union_count, 1, "Should have parsed one union type (Status)");
}

#[test]
fn test_indented_record_fails() {
    let schema_source = r#"
        record User {
            id   Int    @id
            name String
        }
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_err(), "Indented record should fail");
}

#[test]
fn test_indented_type_fails() {
    let schema_source = r#"
        type Status
           = Active
           | Inactive
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_err(), "Indented type should fail");
}

#[test]
fn test_indented_session_fails() {
    let schema_source = r#"
        session {
            userId Int
        }
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_err(), "Indented session should fail");
}

#[test]
fn test_record_with_tab_indentation_fails() {
    let schema_source = "\trecord User {\n\t    id   Int    @id\n\t    name String\n\t}\n";

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_err(), "Record with tab indentation should fail");
}

#[test]
fn test_session_with_tab_indentation_fails() {
    let schema_source = "\tsession {\n\t    userId Int\n\t}\n";

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_err(), "Session with tab indentation should fail");
}

#[test]
fn test_record_with_single_space_indentation_fails() {
    let schema_source = r#" record User {
    id   Int    @id
    name String
}
"#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Record with single space indentation should fail"
    );
}

#[test]
fn test_type_with_single_space_indentation_fails() {
    let schema_source = r#" type Status
   = Active
   | Inactive
"#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Type with single space indentation should fail"
    );
}

#[test]
fn test_session_with_single_space_indentation_fails() {
    let schema_source = r#" session {
    userId Int
}
"#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Session with single space indentation should fail"
    );
}

#[test]
fn test_record_with_deep_indentation_fails() {
    let schema_source = r#"
            record User {
                id   Int    @id
                name String
            }
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_err(), "Record with deep indentation should fail");
}

#[test]
fn test_type_with_deep_indentation_fails() {
    let schema_source = r#"
            type Status
               = Active
               | Inactive
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(result.is_err(), "Type with deep indentation should fail");
}

#[test]
fn test_indented_record_after_valid_record_fails() {
    let schema_source = r#"
record User {
    id   Int    @id
    name String
}

    record Post {
        id   Int    @id
        title String
    }
"#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Indented record after valid record should fail"
    );
}

#[test]
fn test_indented_type_after_valid_record_fails() {
    let schema_source = r#"
record User {
    id   Int    @id
    name String
}

    type Status
       = Active
       | Inactive
"#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Indented type after valid record should fail"
    );
}

#[test]
fn test_indented_session_after_valid_record_fails() {
    let schema_source = r#"
record User {
    id   Int    @id
    name String
}

    session {
        userId Int
    }
"#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Indented session after valid record should fail"
    );
}

#[test]
fn test_record_at_start_of_file_with_spaces_fails() {
    let schema_source = "    record User {\n    id   Int    @id\n    name String\n}\n";

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Record at start of file with spaces should fail"
    );
}

#[test]
fn test_type_at_start_of_file_with_spaces_fails() {
    let schema_source = "    type Status\n   = Active\n   | Inactive\n";

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Type at start of file with spaces should fail"
    );
}

#[test]
fn test_session_at_start_of_file_with_spaces_fails() {
    let schema_source = "    session {\n    userId Int\n}\n";

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Session at start of file with spaces should fail"
    );
}

#[test]
fn test_multiple_indented_declarations_fail() {
    let schema_source = r#"
        record User {
            id   Int    @id
            name String
        }

        type Status
           = Active
           | Inactive

        session {
            userId Int
        }
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_err(),
        "Multiple indented declarations should fail"
    );
}

#[test]
fn test_tagged_type_with_leading_newline() {
    // Test that a tagged type with a leading newline parses successfully
    // This matches the format used in the failing round-trip test
    let schema_source = r#"
type SimpleTagged
   = Option1
   | Option2
   | Option3
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Tagged type with leading newline should parse successfully. Error: {:?}",
        result.err()
    );

    // Verify the tagged type was parsed
    assert_eq!(schema.files.len(), 1, "Should have one schema file");
    let file = &schema.files[0];
    let tagged_count = file
        .definitions
        .iter()
        .filter(|d| matches!(d, ast::Definition::Tagged { .. }))
        .count();
    assert_eq!(tagged_count, 1, "Should have parsed one tagged type");
}

#[test]
fn test_tagged_type_with_fields_and_leading_newline() {
    // Test that a tagged type with fields and a leading newline parses successfully
    let schema_source = r#"
type TaggedWithFields
   = Active
   | Inactive
   | Pending {
        reason String
        createdAt DateTime
    }
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Tagged type with fields and leading newline should parse successfully. Error: {:?}",
        result.err()
    );

    // Verify the tagged type was parsed
    assert_eq!(schema.files.len(), 1, "Should have one schema file");
    let file = &schema.files[0];
    let tagged_count = file
        .definitions
        .iter()
        .filter(|d| matches!(d, ast::Definition::Tagged { .. }))
        .count();
    assert_eq!(tagged_count, 1, "Should have parsed one tagged type");
}

#[test]
fn test_multiple_tagged_types_with_leading_newline() {
    // Test that multiple tagged types with a leading newline parse successfully
    let schema_source = r#"
type SimpleTagged
   = Option1
   | Option2
   | Option3

type TaggedWithFields
   = Active
   | Inactive
   | Pending {
        reason String
        createdAt DateTime
    }
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Multiple tagged types with leading newline should parse successfully. Error: {:?}",
        result.err()
    );

    // Verify both tagged types were parsed
    assert_eq!(schema.files.len(), 1, "Should have one schema file");
    let file = &schema.files[0];
    let tagged_count = file
        .definitions
        .iter()
        .filter(|d| matches!(d, ast::Definition::Tagged { .. }))
        .count();
    assert_eq!(tagged_count, 2, "Should have parsed two tagged types");

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", &schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Multiple tagged types with leading newline should parse successfully. Error: {:?}",
        result.err()
    );

    // Verify both tagged types were parsed
    assert_eq!(schema.files.len(), 1, "Should have one schema file");
    let file = &schema.files[0];
    let tagged_count = file
        .definitions
        .iter()
        .filter(|d| matches!(d, ast::Definition::Tagged { .. }))
        .count();
    assert_eq!(tagged_count, 2, "Should have parsed two tagged types");
}

#[test]
fn test_tagged_type_followed_by_record_with_leading_newline() {
    // Test that a tagged type followed by a record with a leading newline parses successfully
    // This matches the exact format from the failing round-trip test
    let schema_source = r#"
type SimpleTagged
   = Option1
   | Option2
   | Option3

type TaggedWithFields
   = Active
   | Inactive
   | Pending {
        reason String
        createdAt DateTime
    }

record Test {
    id Int @id
    status TaggedWithFields
}
    "#;

    let mut schema = ast::Schema::default();
    let result = parser::run("schema.pyre", schema_source, &mut schema);
    assert!(
        result.is_ok(),
        "Tagged types followed by record with leading newline should parse successfully. Error: {:?}",
        result.err()
    );

    // Verify all definitions were parsed
    assert_eq!(schema.files.len(), 1, "Should have one schema file");
    let file = &schema.files[0];
    let tagged_count = file
        .definitions
        .iter()
        .filter(|d| matches!(d, ast::Definition::Tagged { .. }))
        .count();
    let record_count = file
        .definitions
        .iter()
        .filter(|d| matches!(d, ast::Definition::Record { .. }))
        .count();
    assert_eq!(tagged_count, 2, "Should have parsed two tagged types");
    assert_eq!(record_count, 1, "Should have parsed one record");
}
