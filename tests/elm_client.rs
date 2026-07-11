use pyre::ast;
use pyre::filesystem::GeneratedFile;
use pyre::generate::client::elm;
use pyre::parser;
use pyre::typecheck;
use std::path::Path;

fn path_ends_with(path: &Path, suffix: &str) -> bool {
    path.ends_with(Path::new(suffix))
}

#[test]
fn generated_pyre_elm_uses_query_upserts() {
    let schema_source = r#"
record Rulebook {
    @public

    id   Id.Int @id
    name String
}

record GameWorld {
    @public

    id   Id.Int @id
    slug String
}
"#;

    let query_source = r#"
query GetRulebookByName($name: String) {
    rulebook {
        @where { name == $name }

        id
        name
    }
}

query GetGameWorld($slug: String) {
    gameWorld {
        @where { slug == $slug }

        id
        slug
    }
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema typechecks");

    let query_list = parser::parse_query("query.pyre", query_source).expect("query parses");
    let query_info = typecheck::check_queries(&query_list, &context).expect("query typechecks");

    let mut files: Vec<GeneratedFile<String>> = Vec::new();
    elm::generate_queries(
        &context,
        &query_info,
        &query_list,
        Path::new("client/elm"),
        &mut files,
    );

    let generated = files
        .iter()
        .find(|f| path_ends_with(&f.path, "Pyre.elm"))
        .expect("generated Pyre.elm file");

    let content = &generated.contents;

    assert!(
        content.contains("module Pyre exposing (DatabaseId, Default, QueryId, Model, QueryModel, Query(..), Msg(..), Effect(..), init, update, decodeIncomingDelta, getResult)"),
        "Pyre.elm should expose QueryModel and Query constructors. Generated:\n{}",
        content
    );
    assert!(
        content.contains("type alias DatabaseId namespace =\n    Db.Database.DatabaseId namespace\n\n\ntype alias Default =\n    Db.Database.Default\n\n\ntype alias QueryId =\n    String")
            && content.contains("type Query\n    = GetRulebookByName (DatabaseId Default) QueryId Query.GetRulebookByName.Input\n    | GetGameWorld (DatabaseId Default) QueryId Query.GetGameWorld.Input"),
        "Pyre.elm should generate a Query type for outbound upserts. Generated:\n{}",
        content
    );
    assert!(
        content.contains("type Msg\n    = QueryUpdate Query\n    | GetRulebookByName_DataReceived QueryId Query.GetRulebookByName.QueryDelta\n    | GetRulebookByName_Unregistered (DatabaseId Default) QueryId\n    | GetGameWorld_DataReceived QueryId Query.GetGameWorld.QueryDelta\n    | GetGameWorld_Unregistered (DatabaseId Default) QueryId"),
        "Pyre.elm should collapse register/update into QueryUpdate. Generated:\n{}",
        content
    );
    assert!(
        content.contains("type Effect\n    = NoEffect\n    | Send Encode.Value\n    | QueryUpdated QueryId\n    | LogError Encode.Value"),
        "Pyre.elm should expose query update effects. Generated:\n{}",
        content
    );
    assert!(
        content.contains(", QueryUpdated queryId\n                            )"),
        "Pyre.elm should return QueryUpdated after applying query deltas. Generated:\n{}",
        content
    );
    assert!(
        !content.contains("_Registered") && !content.contains("_InputUpdated"),
        "Pyre.elm should not generate separate register/update constructors anymore. Generated:\n{}",
        content
    );
    assert!(
        content.contains("update msg model =\n    case msg of\n        QueryUpdate query ->\n            updateQuery query model")
            && content.contains("updateQuery : Query -> Model -> ( Model, Effect )\nupdateQuery query model =\n    case query of\n        GetRulebookByName databaseId queryId input ->")
            && content.contains("        GetGameWorld databaseId queryId input ->"),
        "Pyre.elm should delegate QueryUpdate handling to updateQuery. Generated:\n{}",
        content
    );
    assert!(
        content.contains("incomingDeltaDecoder =\n    Decode.map2 Tuple.pair\n        (Decode.field \"queryName\" Decode.string)\n        (Decode.field \"queryId\" Decode.string)")
            && !content.contains("Decode.field \"querySource\" Decode.string"),
        "Pyre.elm should decode queryName, not querySource, for inbound result routing. Generated:\n{}",
        content
    );
    assert!(
        content.contains("Just queryModel ->\n                    ( { model | getRulebookByName = Dict.insert queryId { queryModel | input = input } model.getRulebookByName }\n                    , Send (encodeUpdateInput databaseId queryId Query.GetRulebookByName.queryShape (Query.GetRulebookByName.encode input))")
            && content.contains("Nothing ->\n                    let\n                        queryModel =\n                            { input = input, result = Query.GetRulebookByName.ReturnData [], revision = 0 }\n                    in\n                    ( { model | getRulebookByName = Dict.insert queryId queryModel model.getRulebookByName }\n                    , Send (encodeRegister databaseId \"GetRulebookByName\" Query.GetRulebookByName.queryShape queryId (Query.GetRulebookByName.encode input))")
            && content.contains("encodeRegister : DatabaseId namespace -> String -> Encode.Value -> QueryId -> Encode.Value -> Encode.Value\nencodeRegister databaseId queryName queryShape queryId input =\n    Encode.object\n        [ ( \"type\", Encode.string \"register\" )\n        , ( \"databaseId\", Db.Database.encode databaseId )\n        , ( \"queryName\", Encode.string queryName )\n        , ( \"querySource\", queryShape )")
            && content.contains("encodeUpdateInput : DatabaseId namespace -> QueryId -> Encode.Value -> Encode.Value -> Encode.Value\nencodeUpdateInput databaseId queryId queryShape input =\n    Encode.object\n        [ ( \"type\", Encode.string \"update-input\" )\n        , ( \"databaseId\", Db.Database.encode databaseId )\n        , ( \"queryId\", Encode.string queryId )\n        , ( \"querySource\", queryShape )"),
        "Pyre.elm should upsert queries by id. Generated:\n{}",
        content
    );
}

#[test]
fn generated_elm_mutation_modules_include_bridge_metadata() {
    let schema_source = r#"
record Post {
    @public

    id    Id.Int @id
    title String
}
"#;

    let query_source = r#"
insert CreatePost($title: String) {
    post {
        title = $title
        id
    }
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema typechecks");

    let query_list = parser::parse_query("query.pyre", query_source).expect("query parses");
    let query_info = typecheck::check_queries(&query_list, &context).expect("query typechecks");

    let mut files: Vec<GeneratedFile<String>> = Vec::new();
    elm::generate_queries(
        &context,
        &query_info,
        &query_list,
        Path::new("client/elm"),
        &mut files,
    );

    let generated = files
        .iter()
        .find(|f| path_ends_with(&f.path, "Query/CreatePost.elm"))
        .expect("generated CreatePost.elm file");

    let content = &generated.contents;

    assert!(
        content.contains("module Query.CreatePost exposing (encode, DatabaseId, Default, RequestId, id, name, mutationRequest, decodeMutationResult, MutationResult, Input, Post, ReturnData)"),
        "Mutation modules should expose bridge metadata helpers. Generated:\n{}",
        content
    );
    assert!(
        content.contains("id : String\nid =\n    \"")
            && content.contains("name : String\nname =\n    \"CreatePost\"")
            && content.contains("type alias DatabaseId namespace =\n    Db.Database.DatabaseId namespace\n\n\ntype alias RequestId =\n    String")
            && content.contains("type alias Default =\n    Db.Database.Default")
            && content.contains("mutationRequest : (DatabaseId Default) -> RequestId -> Input -> Encode.Value\nmutationRequest databaseId requestId input =\n    Encode.object\n        [ ( \"type\", Encode.string \"mutate\" )\n        , ( \"databaseId\", Db.Database.encode databaseId )\n        , ( \"requestId\", Encode.string requestId )\n        , ( \"mutationId\", Encode.string id )\n        , ( \"mutationName\", Encode.string name )\n        , ( \"mutationInput\", encode input )\n        ]")
            && content.contains("type alias MutationResult =\n    { requestId : RequestId\n    , mutationId : String\n    , mutationName : Maybe String\n    , result : Result String ReturnData\n    }")
            && content.contains("decodeMutationResult : Decode.Decoder MutationResult")
            && content.contains("decodeBridgeMutationResult : Decode.Decoder value -> Decode.Decoder (Result String value)"),
        "Mutation modules should emit request payloads and typed result decoders. Generated:\n{}",
        content
    );
}

#[test]
fn generated_schema_scoped_entity_stream_modules_encode_id_filtered_streams() {
    let schema_source = r#"
record Post {
    @public

    id    Id.Int @id
    title String
}

record Comment {
    @public

    id     Id.Uuid @id
    postId Post.id
    body   String
}
"#;
    let campaign_schema_source = r#"
record Post {
    @public

    id    Id.Int @id
    title String
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");
    let mut campaign_schema = ast::Schema {
        namespace: "campaign".to_string(),
        ..ast::Schema::default()
    };
    parser::run(
        "campaign.pyre",
        campaign_schema_source,
        &mut campaign_schema,
    )
    .expect("campaign schema parses");

    let database = ast::Database {
        schemas: vec![schema, campaign_schema],
    };

    let mut files: Vec<GeneratedFile<String>> = Vec::new();
    elm::generate(Path::new("client/elm"), &database, &mut files);

    assert!(
        files
            .iter()
            .all(|f| !path_ends_with(&f.path, "EntityStream.elm")),
        "entity streams should be generated per schema, not as one global module"
    );

    let generated = files
        .iter()
        .find(|f| path_ends_with(&f.path, "Db/Stream.elm"))
        .expect("generated default Db.Stream.elm file");

    let content = &generated.contents;

    assert!(
        content.contains("module Db.Stream exposing (DatabaseId, Default, EntityChange(..), EntityChangeBatch, EntityChangeBatchSource(..), EntitySubscription, StreamId, decodeIncomingBatch, register, unregister, comment, post)"),
        "Db.Stream.elm should expose the slim bridge API. Generated:\n{}",
        content
    );
    assert!(
        content.contains(
            "Posts.stream\n                |> Posts.idIn visiblePostIds\n                |> post"
        ),
        "Db.Stream.elm should include succinct module docs with example usage. Generated:\n{}",
        content
    );
    assert!(
        content.contains("type EntitySubscription\n    = EntitySubscription StreamInternal.EntitySubscription")
            && content.contains("comment : StreamInternal.TableSubscription Comments.Stream -> EntitySubscription")
            && content.contains("post : StreamInternal.TableSubscription Posts.Stream -> EntitySubscription"),
        "Db.Stream.elm should generate wrapper constructors for phantom table streams. Generated:\n{}",
        content
    );
    assert!(
        content.contains(
            "register : DatabaseId Default -> StreamId -> List EntitySubscription -> Encode.Value"
        ) && content.contains("( \"type\", Encode.string \"register-entity-stream\" )")
            && content.contains("( \"tables\", Encode.list encodeSubscription subscriptions )"),
        "Db.Stream.elm should encode register-entity-stream payloads. Generated:\n{}",
        content
    );
    assert!(
        content.contains(
            "EntitySubscription inner ->\n            StreamInternal.encodeSubscription inner"
        ),
        "Db.Stream.elm should delegate subscription encoding to the stream internals. Generated:\n{}",
        content
    );
    assert!(
        content.contains("import Db.Table.Comments as Comments")
            && content.contains("import Db.Table.Posts as Posts")
            && !content.contains("type alias PostEntity"),
        "Db.Stream.elm should import table modules instead of defining row aliases. Generated:\n{}",
        content
    );
    assert!(
        content.contains("type EntityChange\n    = CommentRow Comments.Row\n    | PostRow Posts.Row\n    | EntityDecodeFailed String Decode.Value")
            && content.contains("\"comments\" ->\n                        decodeRow CommentRow Comments.decodeRow row")
            && content.contains("\"posts\" ->\n                        decodeRow PostRow Posts.decodeRow row"),
        "Db.Stream.elm should decode batches into typed table row variants. Generated:\n{}",
        content
    );

    let post_table = files
        .iter()
        .find(|f| path_ends_with(&f.path, "Db/Table/Posts.elm"))
        .expect("generated default posts table module");
    assert!(
        post_table
            .contents
            .contains("module Db.Table.Posts exposing (Row, Stream, decodeRow, stream, idIn)")
            && post_table
                .contents
                .contains("type alias Row =\n    { id : Int\n    , title : String\n    }")
            && post_table
                .contents
                .contains("stream : StreamInternal.TableSubscription Stream\nstream =\n    StreamInternal.table \"posts\"")
            && post_table.contents.contains("idIn : List Int -> StreamInternal.TableSubscription Stream -> StreamInternal.TableSubscription Stream")
            && post_table.contents.contains("StreamInternal.addCondition \"id\" (Encode.object [ ( \"$in\", Encode.list Encode.int values ) ]) subscription")
            && post_table
                .contents
                .contains("decodeRow : Decode.Decoder Row\ndecodeRow =\n    Decode.succeed Row")
            && post_table
                .contents
                .contains("|> Db.Decode.andField \"id\" Decode.int"),
        "Db.Table.Posts should expose the row type and decoder. Generated:\n{}",
        post_table.contents
    );

    let comment_table = files
        .iter()
        .find(|f| path_ends_with(&f.path, "Db/Table/Comments.elm"))
        .expect("generated default comments table module");
    assert!(
        comment_table
            .contents
            .contains("module Db.Table.Comments exposing (Row, Stream, decodeRow, stream, idIn, postIdIn)")
            && comment_table.contents.contains(
            "type alias Row =\n    { id : String\n    , postId : Int\n    , body : String\n    }"
        ) && comment_table
            .contents
            .contains("|> Db.Decode.andField \"postId\" Decode.int")
            && comment_table.contents.contains("postIdIn : List Int -> StreamInternal.TableSubscription Stream -> StreamInternal.TableSubscription Stream"),
        "Db.Table.Comments should expose the row type and decoder. Generated:\n{}",
        comment_table.contents
    );

    let campaign_stream = files
        .iter()
        .find(|f| path_ends_with(&f.path, "Db/Campaign/Stream.elm"))
        .expect("generated campaign stream module");
    assert!(
        campaign_stream
            .contents
            .contains("module Db.Campaign.Stream exposing (DatabaseId, Campaign, EntityChange(..), EntityChangeBatch, EntityChangeBatchSource(..), EntitySubscription, StreamId, decodeIncomingBatch, register, unregister, post)")
            && campaign_stream
                .contents
                .contains("import Db.Campaign.Table.Posts as Posts")
            && campaign_stream
                .contents
                .contains("register : DatabaseId Campaign -> StreamId -> List EntitySubscription -> Encode.Value"),
        "named schema should generate a schema-scoped stream module. Generated:\n{}",
        campaign_stream.contents
    );

    let campaign_post_table = files
        .iter()
        .find(|f| path_ends_with(&f.path, "Db/Campaign/Table/Posts.elm"))
        .expect("generated campaign posts table module");
    assert!(
        campaign_post_table.contents.contains(
            "module Db.Campaign.Table.Posts exposing (Row, Stream, decodeRow, stream, idIn)"
        ),
        "named schema should generate schema-scoped table modules. Generated:\n{}",
        campaign_post_table.contents
    );
}

#[test]
fn generated_elm_datetime_decodes_and_encodes_unix_seconds() {
    let schema_source = r#"
record Clocktower {
    @public

    id        Id.Uuid @id
    startedAt DateTime
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");

    let database = ast::Database {
        schemas: vec![schema],
    };

    let mut files: Vec<GeneratedFile<String>> = Vec::new();
    elm::generate(Path::new("client/elm"), &database, &mut files);

    let decode = files
        .iter()
        .find(|f| path_ends_with(&f.path, "Db/Decode.elm"))
        .expect("generated Db.Decode.elm file");
    assert!(
        decode.contents.contains(
            "dateTime =\n    Decode.map (\\seconds -> Time.millisToPosix (seconds * 1000)) Decode.int"
        ),
        "Db.Decode.dateTime should decode unix seconds. Generated:\n{}",
        decode.contents
    );

    let encode = files
        .iter()
        .find(|f| path_ends_with(&f.path, "Db/Encode.elm"))
        .expect("generated Db.Encode.elm file");
    assert!(
        encode
            .contents
            .contains("Encode.int (Time.posixToMillis time // 1000)"),
        "Db.Encode.dateTime should serialize as unix seconds. Generated:\n{}",
        encode.contents
    );

    let table = files
        .iter()
        .find(|f| path_ends_with(&f.path, "Db/Table/Clocktowers.elm"))
        .expect("generated clocktowers table module");
    assert!(
        table
            .contents
            .contains("|> Db.Decode.andField \"startedAt\" Db.Decode.dateTime"),
        "entity stream rows should use the DateTime decoder. Generated:\n{}",
        table.contents
    );
}
