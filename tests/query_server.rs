#[allow(dead_code, unused_imports)]
mod helpers;

use helpers::test_database::TestDatabase;
use pyre::server::manifest::{Manifest, PyreSession, QueryManifest};
use pyre::server::query;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::process::Command;

fn manifest_for(
    context: &pyre::typecheck::Context,
    query_source: &str,
    include_generated_crud: bool,
) -> Result<Manifest, Box<dyn std::error::Error>> {
    let mut query_list = if query_source.trim().is_empty() {
        pyre::ast::QueryList {
            queries: Vec::new(),
        }
    } else {
        pyre::parser::parse_query("query.pyre", query_source)
            .map_err(|err| format!("query parse failed: {:?}", err))?
    };

    if include_generated_crud {
        pyre::generated_queries::append_generated_crud_queries(&mut query_list, context);
    }

    let query_info = pyre::typecheck::check_queries(&query_list, context)
        .map_err(|errors| format!("query typecheck failed: {:?}", errors))?;
    let mut files = Vec::new();
    pyre::generate::manifest::generate_queries(context, &query_list, &query_info, &mut files);
    let manifest_file = files
        .into_iter()
        .find(|file| file.path == std::path::Path::new("manifest.json"))
        .ok_or("manifest file should be generated")?;

    Ok(serde_json::from_str(&manifest_file.contents)?)
}

fn only_query(manifest: &Manifest) -> &QueryManifest {
    manifest
        .queries
        .values()
        .next()
        .expect("manifest should contain a query")
}

#[tokio::test]
async fn run_query_accepts_uuid_record_id_parameter() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record ClocktowerGame {
    @public
    id Id.Uuid @id
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let id = "ab47fc00-f638-4ffd-8b08-4773181d6c3f";
    let manifest = manifest_for(
        &db.context,
        r#"
query ClocktowerGameKeystone($id: ClocktowerGame.id) {
    clocktowerGame {
        @where { id == $id }
        id
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let result = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "id": id }),
        &session,
    )
    .await?;

    assert!(result.response["clocktowerGame"].is_array());
    Ok(())
}

fn query_by_operation<'a>(manifest: &'a Manifest, operation: &str) -> &'a QueryManifest {
    manifest
        .queries
        .values()
        .find(|query| query.operation == operation)
        .expect("query should exist for operation")
}

fn query_by_input_type<'a>(
    manifest: &'a Manifest,
    input_name: &str,
    input_type: &str,
) -> &'a QueryManifest {
    manifest
        .queries
        .values()
        .find(|query| {
            query
                .input_schema
                .get(input_name)
                .is_some_and(|schema| schema.type_ == input_type)
        })
        .expect("query should have an input with the expected type")
}

fn query_by_input_names<'a>(manifest: &'a Manifest, names: &[&str]) -> &'a QueryManifest {
    manifest
        .queries
        .values()
        .find(|query| {
            query.input_schema.len() == names.len()
                && names
                    .iter()
                    .all(|name| query.input_schema.contains_key(*name))
        })
        .expect("query should have the expected input fields")
}

fn query_by_input_signature<'a>(
    manifest: &'a Manifest,
    names: &[&str],
    typed_name: &str,
    typed_value: &str,
) -> &'a QueryManifest {
    manifest
        .queries
        .values()
        .find(|query| {
            query.input_schema.len() == names.len()
                && query
                    .input_schema
                    .get(typed_name)
                    .is_some_and(|schema| schema.type_ == typed_value)
                && names
                    .iter()
                    .all(|name| query.input_schema.contains_key(*name))
        })
        .expect("query should have the expected typed input fields")
}

fn query_by_nullable_input<'a>(
    manifest: &'a Manifest,
    name: &str,
    type_: &str,
    nullable: bool,
) -> &'a QueryManifest {
    manifest
        .queries
        .values()
        .find(|query| {
            query.input_schema.len() == 1
                && query
                    .input_schema
                    .get(name)
                    .is_some_and(|schema| schema.type_ == type_ && schema.nullable == nullable)
        })
        .expect("query should have the expected nullable input")
}

#[tokio::test]
async fn generated_manifest_distinguishes_bool_from_unit_enum(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
type Status
    = Running
    | Stopped

record Game {
    @public
    id Id.Int @id
    isAdmin Bool
    status Status
}
"#,
    )
    .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
insert CreateGame($isAdmin: Bool, $status: Status) {
    game {
        isAdmin = $isAdmin
        status = $status
    }
}
"#,
        false,
    )?;
    let query = only_query(&manifest);
    assert!(!query.input_schema["isAdmin"].is_enum);
    assert!(query.input_schema["isAdmin"].enum_variants.is_empty());
    assert!(query.input_schema["status"].is_enum);
    assert_eq!(
        query.input_schema["status"].enum_variants,
        vec!["Running", "Stopped"]
    );

    Ok(())
}

fn generated_rust_server(
    context: &pyre::typecheck::Context,
    query_source: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let query_list = pyre::parser::parse_query("query.pyre", query_source).map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{error:?}"))
    })?;
    let mut files = Vec::new();
    pyre::generate::server::rust::generate_queries(
        context,
        &query_list,
        Path::new("rust"),
        &mut files,
    );

    files
        .into_iter()
        .find(|file| file.path == Path::new("rust/server.rs"))
        .map(|file| file.contents)
        .ok_or_else(|| "generated Rust server file is missing".into())
}

fn compile_and_run_generated_rust(
    server: &str,
    responses: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir)?;
    fs::write(
        temp_dir.path().join("Cargo.toml"),
        r#"
[package]
name = "pyre-generated-rust-test"
version = "0.0.0"
edition = "2021"

[dependencies]
serde = { version = "1.0.203", features = ["derive"] }
serde_json = "1.0.117"
serde_path_to_error = "0.1.20"
"#,
    )?;
    fs::write(src_dir.join("server.rs"), server)?;
    fs::write(
        src_dir.join("main.rs"),
        r#"
mod server;

fn main() {
    use std::convert::TryFrom;

    let game_running = server::create_clocktower_game::Input {
        status: server::ClocktowerGameStatus::ClocktowerRunning,
    };
    assert_eq!(
        game_running.into_json(),
        serde_json::json!({ "status": { "_type": "ClocktowerRunning" } })
    );
    let game_stopped = server::create_clocktower_game::Input {
        status: server::ClocktowerGameStatus::ClocktowerStopped,
    };
    assert_eq!(
        game_stopped.into_json(),
        serde_json::json!({ "status": { "_type": "ClocktowerStopped" } })
    );
    let participant_approved = server::create_clocktower_participant::Input {
        status: server::ClocktowerParticipantStatus::ClocktowerParticipantApproved,
    };
    assert_eq!(
        participant_approved.into_json(),
        serde_json::json!({ "status": { "_type": "ClocktowerParticipantApproved" } })
    );
    let participant_rejected = server::create_clocktower_participant::Input {
        status: server::ClocktowerParticipantStatus::ClocktowerParticipantRejected,
    };
    assert_eq!(
        participant_rejected.into_json(),
        serde_json::json!({ "status": { "_type": "ClocktowerParticipantRejected" } })
    );
    let scene_hidden = server::create_scene::Input {
        visibility: server::Visibility::Hidden,
    };
    assert_eq!(
        scene_hidden.into_json(),
        serde_json::json!({ "visibility": { "_type": "Hidden" } })
    );
    let scene_users = server::create_scene::Input {
        visibility: server::Visibility::Users { user_id: 7 },
    };
    assert_eq!(
        scene_users.into_json(),
        serde_json::json!({ "visibility": { "_type": "Users", "userId": 7 } })
    );
    let task_create = server::create_task::Input {
        action: server::Action::Create { title: "draft".to_string() },
    };
    assert_eq!(
        task_create.into_json(),
        serde_json::json!({ "action": { "_type": "Create", "title": "draft" } })
    );
    let task_delete = server::create_task::Input {
        action: server::Action::Delete { id: 7 },
    };
    assert_eq!(
        task_delete.into_json(),
        serde_json::json!({ "action": { "_type": "Delete", "id": 7 } })
    );
    let message = server::create_message::Input {
        envelope: server::Envelope::Wrapped {
            detail: server::Detail::Note { message: None },
        },
    };
    assert_eq!(
        message.into_json(),
        serde_json::json!({
            "envelope": { "_type": "Wrapped", "detail": { "_type": "Note", "message": null } }
        })
    );
    let archive = server::create_archive::Input {
        actions: vec![
            server::Action::Create { title: "first".to_string() },
            server::Action::Delete { id: 3 },
        ],
        actions_by_name: std::collections::HashMap::from([
            ("draft".to_string(), server::Action::Create { title: "second".to_string() }),
            ("removed".to_string(), server::Action::Delete { id: 4 }),
        ]),
    };
    assert_eq!(
        archive.into_json(),
        serde_json::json!({
            "actions": [
                { "_type": "Create", "title": "first" },
                { "_type": "Delete", "id": 3 }
            ],
            "actionsByName": {
                "draft": { "_type": "Create", "title": "second" },
                "removed": { "_type": "Delete", "id": 4 }
            }
        })
    );
    let update_task = server::update_task::Input {
        id: 1,
        action: server::Action::Delete { id: 9 },
    };
    assert_eq!(
        update_task.into_json(),
        serde_json::json!({ "id": 1, "action": { "_type": "Delete", "id": 9 } })
    );
    let delete_task = server::delete_task::Input { id: 2 };
    assert_eq!(delete_task.into_json(), serde_json::json!({ "id": 2 }));
    let event_seconds = server::create_event::Input {
        starts_at: server::DateTime::UnixSeconds(123),
    };
    assert_eq!(event_seconds.into_json(), serde_json::json!({ "startsAt": 123 }));
    let event_text = server::create_event::Input {
        starts_at: server::DateTime::Text("2026-01-02 03:04:05".to_string()),
    };
    assert_eq!(
        event_text.into_json(),
        serde_json::json!({ "startsAt": "2026-01-02 03:04:05" })
    );
    let uuid_game = server::get_uuid_game::Input {
        id: "00000000-0000-0000-0000-000000000001".to_string(),
    };
    assert_eq!(
        uuid_game.into_json(),
        serde_json::json!({ "id": "00000000-0000-0000-0000-000000000001" })
    );
    let delete_game = server::delete_clocktower_game::Input { game_id: 2 };
    assert_eq!(delete_game.into_json(), serde_json::json!({ "gameId": 2 }));
    let nullable_game = server::create_nullable_game::Input {
        status: Some(server::ClocktowerGameStatus::ClocktowerRunning),
    };
    assert_eq!(
        nullable_game.into_json(),
        serde_json::json!({ "status": { "_type": "ClocktowerRunning" } })
    );
    let nullable_game = server::create_nullable_game::Input { status: None };
    assert_eq!(nullable_game.into_json(), serde_json::json!({ "status": null }));
    let update_game = server::update_clocktower_game::Input {
        id: 1,
        status: server::ClocktowerGameStatus::ClocktowerStopped,
    };
    assert_eq!(
        update_game.into_json(),
        serde_json::json!({ "id": 1, "status": { "_type": "ClocktowerStopped" } })
    );

    let responses: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(std::env::args().nth(1).expect("response path"))
            .expect("read responses"),
    )
    .expect("parse responses");
    let response = |name| responses.get(name).cloned().expect("named response");
    let game_running = server::create_clocktower_game::Output::try_from(response("game_running"))
        .expect("generated running game output decodes");
    assert!(matches!(
        &game_running.clocktower_game[0].status,
        server::ClocktowerGameStatus::ClocktowerRunning
    ));
    let game_stopped = server::create_clocktower_game::Output::try_from(response("game_stopped"))
        .expect("generated stopped game output decodes");
    assert!(matches!(
        &game_stopped.clocktower_game[0].status,
        server::ClocktowerGameStatus::ClocktowerStopped
    ));
    let participant_approved =
        server::create_clocktower_participant::Output::try_from(response("participant_approved"))
            .expect("generated approved participant output decodes");
    assert!(matches!(
        &participant_approved.clocktower_participant[0].status,
        server::ClocktowerParticipantStatus::ClocktowerParticipantApproved
    ));
    let participant_rejected =
        server::create_clocktower_participant::Output::try_from(response("participant_rejected"))
            .expect("generated rejected participant output decodes");
    assert!(matches!(
        &participant_rejected.clocktower_participant[0].status,
        server::ClocktowerParticipantStatus::ClocktowerParticipantRejected
    ));
    let scene_hidden = server::create_scene::Output::try_from(response("scene_hidden"))
        .expect("generated hidden scene output decodes");
    assert!(matches!(
        &scene_hidden.scene[0].visibility,
        server::Visibility::Hidden
    ));
    let scene_users = server::create_scene::Output::try_from(response("scene_users"))
        .expect("generated users scene output decodes");
    match &scene_users.scene[0].visibility {
        server::Visibility::Users { user_id } => assert_eq!(*user_id, 7),
        _ => panic!("generated users scene output decoded the wrong variant"),
    }
    let task_create = server::create_task::Output::try_from(response("task_create"))
        .expect("generated create task output decodes");
    match &task_create.task[0].action {
        server::Action::Create { title } => assert_eq!(title, "draft"),
        _ => panic!("generated create task output decoded the wrong variant"),
    }
    let task_delete = server::create_task::Output::try_from(response("task_delete"))
        .expect("generated delete task output decodes");
    match &task_delete.task[0].action {
        server::Action::Delete { id } => assert_eq!(*id, 7),
        _ => panic!("generated delete task output decoded the wrong variant"),
    }
    let message = server::create_message::Output::try_from(response("message_null"))
        .expect("generated message output decodes");
    match &message.message[0].envelope {
        server::Envelope::Wrapped {
            detail: server::Detail::Note { message },
        } => assert_eq!(message, &None),
        _ => panic!("generated message output decoded the wrong nested variant"),
    }
    let archive = server::create_archive::Output::try_from(response("archive"))
        .expect("generated archive output decodes");
    assert!(matches!(
        &archive.archive[0].actions[..],
        [server::Action::Create { title }, server::Action::Delete { id: 3 }] if title == "first"
    ));
    match &archive.archive[0].actions_by_name["draft"] {
        server::Action::Create { title } => assert_eq!(title, "second"),
        _ => panic!("generated archive dictionary decoded the wrong variant"),
    }
    let updated_task = server::update_task::Output::try_from(response("task_updated"))
        .expect("generated updated task output decodes");
    assert!(matches!(
        &updated_task.task[0].action,
        server::Action::Delete { id: 9 }
    ));
    let deleted_task = server::delete_task::Output::try_from(response("task_deleted"))
        .expect("generated deleted task output decodes");
    assert!(matches!(
        &deleted_task.task[0].action,
        server::Action::Delete { id: 7 }
    ));
    let listed_games = server::list_clocktower_games::Output::try_from(response("games_listed"))
        .expect("generated game query output decodes");
    assert!(matches!(
        &listed_games.clocktower_game[..],
        [
            server::list_clocktower_games::ClocktowerGame { status: server::ClocktowerGameStatus::ClocktowerRunning },
            server::list_clocktower_games::ClocktowerGame { status: server::ClocktowerGameStatus::ClocktowerStopped },
            server::list_clocktower_games::ClocktowerGame { status: server::ClocktowerGameStatus::ClocktowerRunning }
        ]
    ));
    let updated_game = server::update_clocktower_game::Output::try_from(response("game_updated"))
        .expect("generated unit enum update output decodes");
    assert!(matches!(
        &updated_game.clocktower_game[0].status,
        server::ClocktowerGameStatus::ClocktowerStopped
    ));
    let event = server::create_event::Output::try_from(response("event_seconds"))
        .expect("generated event output decodes");
    assert!(matches!(
        &event.event[0].starts_at,
        server::DateTime::UnixSeconds(123)
    ));
    let uuid_game = server::get_uuid_game::Output::try_from(response("uuid_game"))
        .expect("generated UUID game output decodes");
    assert_eq!(uuid_game.uuid_game[0].id, "00000000-0000-0000-0000-000000000001");
    let event_text = server::create_event::Output::try_from(response("event_text"))
        .expect("generated text event output decodes");
    assert!(matches!(
        &event_text.event[0].starts_at,
        server::DateTime::Text(value) if value == "2026-01-02 03:04:05"
    ));
    let deleted_game = server::delete_clocktower_game::Output::try_from(response("game_deleted"))
        .expect("generated unit enum delete output decodes");
    assert!(matches!(
        &deleted_game.clocktower_game[0].status,
        server::ClocktowerGameStatus::ClocktowerStopped
    ));
    let nullable_game = server::create_nullable_game::Output::try_from(response("nullable_game_running"))
        .expect("generated nullable unit enum output decodes");
    assert!(matches!(
        &nullable_game.nullable_game[0].status,
        Some(server::ClocktowerGameStatus::ClocktowerRunning)
    ));
    let nullable_game = server::create_nullable_game::Output::try_from(response("nullable_game_null"))
        .expect("generated null unit enum output decodes");
    assert!(nullable_game.nullable_game[0].status.is_none());
}
"#,
    )?;
    fs::write(
        temp_dir.path().join("responses.json"),
        serde_json::to_string(responses)?,
    )?;

    let output = Command::new("cargo")
        .args(["run", "--offline", "--quiet", "--manifest-path"])
        .arg(temp_dir.path().join("Cargo.toml"))
        .arg("--")
        .arg(temp_dir.path().join("responses.json"))
        .output()?;
    if output.status.success() {
        return Ok(());
    }

    Err(std::io::Error::other(format!(
        "generated Rust client failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
    .into())
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "_type")]
enum ClocktowerGameStatus {
    ClocktowerRunning,
    ClocktowerStopped,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "_type")]
enum ClocktowerParticipantStatus {
    ClocktowerParticipantApproved,
    ClocktowerParticipantRejected,
}

#[derive(Serialize)]
struct ClocktowerGameCreateInput {
    status: ClocktowerGameStatus,
}

#[derive(Serialize)]
struct ClocktowerParticipantCreateInput {
    status: ClocktowerParticipantStatus,
}

#[derive(Deserialize)]
struct ClocktowerGame {
    status: ClocktowerGameStatus,
}

#[derive(Deserialize)]
struct ClocktowerParticipant {
    status: ClocktowerParticipantStatus,
}

#[derive(Deserialize)]
struct ClocktowerGameCreateOutput {
    #[serde(rename = "clocktowerGame")]
    clocktower_game: Vec<ClocktowerGame>,
}

#[derive(Deserialize)]
struct ClocktowerParticipantCreateOutput {
    #[serde(rename = "clocktowerParticipant")]
    clocktower_participant: Vec<ClocktowerParticipant>,
}

#[tokio::test]
async fn run_mutations_roundtrip_tagged_union_variants() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
type ClocktowerGameStatus
    = ClocktowerRunning
    | ClocktowerStopped

type ClocktowerParticipantStatus
    = ClocktowerParticipantApproved
    | ClocktowerParticipantRejected

type Visibility
    = Hidden
    | Users { userId Int }

type Action
    = Create { title String }
    | Delete { id Int }

type Detail
    = Note { message String? }

type Envelope
    = Wrapped { detail Detail }

record ClocktowerGame {
    @public
    id Id.Int @id
    status ClocktowerGameStatus
}

record ClocktowerParticipant {
    @public
    id Id.Int @id
    status ClocktowerParticipantStatus
}

record Scene {
    @public
    id Id.Int @id
    visibility Visibility
}

record Task {
    @public
    id Id.Int @id
    action Action
}

record Message {
    @public
    id Id.Int @id
    envelope Envelope
}

record Archive {
    @public
    id Id.Int @id
    actions Json<List<Action>>
    actionsByName Json<Dict<Action>>
}

record Event {
    @public
    id Id.Int @id
    startsAt DateTime
}

record UuidGame {
    @public
    id Id.Uuid @id
}

record NullableGame {
    @public
    id Id.Int @id
    status ClocktowerGameStatus?
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let query_source = r#"
insert CreateClocktowerGame($status: ClocktowerGameStatus) {
    clocktowerGame {
        status = $status
    }
}

insert CreateClocktowerParticipant($status: ClocktowerParticipantStatus) {
    clocktowerParticipant {
        status = $status
    }
}

query ListClocktowerGames {
    clocktowerGame {
        status
    }
}

update UpdateClocktowerGame($id: ClocktowerGame.id, $status: ClocktowerGameStatus) {
    clocktowerGame {
        @where { id == $id }
        status = $status
    }
}

insert CreateScene($visibility: Visibility) {
    scene {
        visibility = $visibility
    }
}

insert CreateTask($action: Action) {
    task {
        action = $action
    }
}

insert CreateMessage($envelope: Envelope) {
    message {
        envelope = $envelope
    }
}

insert CreateArchive($actions: Json<List<Action>>, $actionsByName: Json<Dict<Action>>) {
    archive {
        actions = $actions
        actionsByName = $actionsByName
    }
}

update UpdateTask($id: Task.id, $action: Action) {
    task {
        @where { id == $id }
        action = $action
    }
}

delete DeleteTask($id: Task.id) {
    task {
        @where { id == $id }
        action
    }
}

insert CreateEvent($startsAt: DateTime) {
    event {
        startsAt = $startsAt
    }
}

query GetUuidGame($id: UuidGame.id) {
    uuidGame {
        @where { id == $id }
        id
    }
}

delete DeleteClocktowerGame($gameId: ClocktowerGame.id) {
    clocktowerGame {
        @where { id == $gameId }
        status
    }
}

insert CreateNullableGame($status: ClocktowerGameStatus?) {
    nullableGame {
        status = $status
    }
}
"#;
    let manifest = manifest_for(&db.context, query_source, false)?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let mut responses = serde_json::Map::new();

    for (name, status, tag) in [
        (
            "game_running",
            ClocktowerGameStatus::ClocktowerRunning,
            "ClocktowerRunning",
        ),
        (
            "game_stopped",
            ClocktowerGameStatus::ClocktowerStopped,
            "ClocktowerStopped",
        ),
    ] {
        let input = serde_json::to_value(ClocktowerGameCreateInput {
            status: status.clone(),
        })?;
        assert_eq!(input["status"]["_type"], json!(tag));
        let result = query::run(
            &conn,
            &manifest,
            &query_by_nullable_input(&manifest, "status", "ClocktowerGameStatus", false).id,
            input,
            &session,
        )
        .await?;
        let output: ClocktowerGameCreateOutput = serde_json::from_value(result.response.clone())?;
        assert_eq!(output.clocktower_game[0].status, status);
        responses.insert(name.to_string(), result.response);
    }

    let mut rows = conn
        .query("select status from clocktowerGames order by id", ())
        .await?;
    let mut stored_game_statuses = Vec::new();
    while let Some(row) = rows.next().await? {
        stored_game_statuses.push(row.get::<String>(0)?);
    }
    assert_eq!(
        stored_game_statuses,
        vec!["ClocktowerRunning", "ClocktowerStopped"]
    );

    for invalid_status in [
        json!({}),
        json!({ "_type": 1 }),
        json!({ "_type": "Unknown" }),
    ] {
        assert!(
            query::run(
                &conn,
                &manifest,
                &query_by_nullable_input(&manifest, "status", "ClocktowerGameStatus", false).id,
                json!({ "status": invalid_status }),
                &session,
            )
            .await
            .is_err(),
            "invalid enum input should be rejected"
        );
    }

    let legacy_result = query::run(
        &conn,
        &manifest,
        &query_by_nullable_input(&manifest, "status", "ClocktowerGameStatus", false).id,
        json!({ "status": "ClocktowerRunning" }),
        &session,
    )
    .await?;
    assert_eq!(
        legacy_result.response["clocktowerGame"][0]["status"],
        json!({ "_type": "ClocktowerRunning" })
    );

    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_names(&manifest, &[]).id,
        json!({}),
        &session,
    )
    .await?;
    let listed_game_response = result.response;

    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_signature(
            &manifest,
            &["id", "status"],
            "status",
            "ClocktowerGameStatus",
        )
        .id,
        json!({ "id": 1, "status": { "_type": "ClocktowerStopped" } }),
        &session,
    )
    .await?;
    let updated_game_response = result.response;

    for (name, status, tag) in [
        (
            "participant_approved",
            ClocktowerParticipantStatus::ClocktowerParticipantApproved,
            "ClocktowerParticipantApproved",
        ),
        (
            "participant_rejected",
            ClocktowerParticipantStatus::ClocktowerParticipantRejected,
            "ClocktowerParticipantRejected",
        ),
    ] {
        let input = serde_json::to_value(ClocktowerParticipantCreateInput {
            status: status.clone(),
        })?;
        assert_eq!(input["status"]["_type"], json!(tag));
        let result = query::run(
            &conn,
            &manifest,
            &query_by_input_type(&manifest, "status", "ClocktowerParticipantStatus").id,
            input,
            &session,
        )
        .await?;
        let output: ClocktowerParticipantCreateOutput =
            serde_json::from_value(result.response.clone())?;
        assert_eq!(output.clocktower_participant[0].status, status);
        responses.insert(name.to_string(), result.response);
    }

    let mut rows = conn
        .query("select status from clocktowerParticipants order by id", ())
        .await?;
    let mut stored_statuses = Vec::new();
    while let Some(row) = rows.next().await? {
        stored_statuses.push(row.get::<String>(0)?);
    }
    assert_eq!(
        stored_statuses,
        vec![
            "ClocktowerParticipantApproved",
            "ClocktowerParticipantRejected"
        ]
    );

    for (name, visibility) in [
        ("scene_hidden", json!({ "_type": "Hidden" })),
        ("scene_users", json!({ "_type": "Users", "userId": 7 })),
    ] {
        let result = query::run(
            &conn,
            &manifest,
            &query_by_input_type(&manifest, "visibility", "Visibility").id,
            json!({ "visibility": visibility.clone() }),
            &session,
        )
        .await?;
        assert_eq!(result.response["scene"][0]["visibility"], visibility);
        responses.insert(name.to_string(), result.response);
    }

    for (name, action) in [
        (
            "task_create",
            json!({ "_type": "Create", "title": "draft" }),
        ),
        ("task_delete", json!({ "_type": "Delete", "id": 7 })),
    ] {
        let result = query::run(
            &conn,
            &manifest,
            &query_by_input_names(&manifest, &["action"]).id,
            json!({ "action": action.clone() }),
            &session,
        )
        .await?;
        assert_eq!(result.response["task"][0]["action"], action);
        responses.insert(name.to_string(), result.response);
    }

    let envelope = json!({
        "_type": "Wrapped",
        "detail": { "_type": "Note", "message": null }
    });
    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_type(&manifest, "envelope", "Envelope").id,
        json!({ "envelope": envelope.clone() }),
        &session,
    )
    .await?;
    assert_eq!(result.response["message"][0]["envelope"], envelope);
    responses.insert("message_null".to_string(), result.response);

    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_type(&manifest, "startsAt", "DateTime").id,
        json!({ "startsAt": "2026-01-02 03:04:05" }),
        &session,
    )
    .await?;
    responses.insert("event_text".to_string(), result.response);

    let actions = json!([
        { "_type": "Create", "title": "first" },
        { "_type": "Delete", "id": 3 }
    ]);
    let actions_by_name = json!({
        "draft": { "_type": "Create", "title": "second" },
        "removed": { "_type": "Delete", "id": 4 }
    });
    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_type(&manifest, "actions", "Json<List<Action>>").id,
        json!({ "actions": actions.clone(), "actionsByName": actions_by_name.clone() }),
        &session,
    )
    .await?;
    assert_eq!(result.response["archive"][0]["actions"], actions);
    assert_eq!(
        result.response["archive"][0]["actionsByName"],
        actions_by_name
    );
    responses.insert("archive".to_string(), result.response);

    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_names(&manifest, &["gameId"]).id,
        json!({ "gameId": 2 }),
        &session,
    )
    .await?;
    responses.insert("game_deleted".to_string(), result.response);

    for (name, status) in [
        (
            "nullable_game_running",
            json!({ "_type": "ClocktowerRunning" }),
        ),
        ("nullable_game_null", serde_json::Value::Null),
    ] {
        let result = query::run(
            &conn,
            &manifest,
            &query_by_nullable_input(&manifest, "status", "ClocktowerGameStatus", true).id,
            json!({ "status": status }),
            &session,
        )
        .await?;
        responses.insert(name.to_string(), result.response);
    }

    let updated_action = json!({ "_type": "Delete", "id": 9 });
    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_signature(&manifest, &["id", "action"], "action", "Action").id,
        json!({ "id": 1, "action": updated_action.clone() }),
        &session,
    )
    .await?;
    assert_eq!(result.response["task"][0]["action"], updated_action);
    responses.insert("task_updated".to_string(), result.response);

    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_signature(&manifest, &["id"], "id", "Id.Int").id,
        json!({ "id": 2 }),
        &session,
    )
    .await?;
    assert_eq!(
        result.response["task"][0]["action"],
        json!({ "_type": "Delete", "id": 7 })
    );
    responses.insert("task_deleted".to_string(), result.response);

    responses.insert("games_listed".to_string(), listed_game_response);
    responses.insert("game_updated".to_string(), updated_game_response);

    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_type(&manifest, "startsAt", "DateTime").id,
        json!({ "startsAt": 123 }),
        &session,
    )
    .await?;
    responses.insert("event_seconds".to_string(), result.response);

    let uuid = "00000000-0000-0000-0000-000000000001";
    conn.execute(
        "insert into uuidGames (id) values (?)",
        libsql::params![uuid],
    )
    .await?;
    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_type(&manifest, "id", "Id.Uuid").id,
        json!({ "id": uuid }),
        &session,
    )
    .await?;
    responses.insert("uuid_game".to_string(), result.response);

    compile_and_run_generated_rust(
        &generated_rust_server(&db.context, query_source)?,
        &responses,
    )?;

    Ok(())
}

#[tokio::test]
async fn run_query_binds_tagged_enum_session_values() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
type Role
    = Admin
    | Member

session {
    role Role
}

record Note {
    id Id.Int @id
    role Role
    body String
    @allow(*) { role == Session.role }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch(
        "insert into notes (id, role, body) values (1, 'Admin', 'one'), (2, 'Member', 'two');",
    )
    .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
query ListNotes {
    note {
        id
    }
}

update UpdateNote($id: Note.id, $body: String) {
    note {
        @where { id == $id }
        body = $body
    }
}

delete DeleteNote($id: Note.id) {
    note {
        @where { id == $id }
        id
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(
        json!({ "role": { "_type": "Admin" } }),
        &manifest.session_schema,
    )?;
    assert_eq!(session.sql_args()["session_role"], json!("Admin"));

    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_names(&manifest, &[]).id,
        json!({}),
        &session,
    )
    .await?;
    assert_eq!(result.response["note"], json!([{ "id": 1 }]));

    query::run(
        &conn,
        &manifest,
        &query_by_operation(&manifest, "update").id,
        json!({ "id": 1, "body": "updated" }),
        &session,
    )
    .await?;
    let mut rows = conn
        .query("select body from notes where id = 1", ())
        .await?;
    assert_eq!(
        rows.next()
            .await?
            .expect("admin note exists")
            .get::<String>(0)?,
        "updated"
    );
    query::run(
        &conn,
        &manifest,
        &query_by_operation(&manifest, "delete").id,
        json!({ "id": 1 }),
        &session,
    )
    .await?;

    let member_session = PyreSession::new(
        json!({ "role": { "_type": "Member" } }),
        &manifest.session_schema,
    )?;
    let result = query::run(
        &conn,
        &manifest,
        &query_by_input_names(&manifest, &[]).id,
        json!({}),
        &member_session,
    )
    .await?;
    assert_eq!(result.response["note"], json!([{ "id": 2 }]));
    assert!(PyreSession::new(
        json!({ "role": { "_type": "Unknown" } }),
        &manifest.session_schema,
    )
    .is_err());

    Ok(())
}

#[tokio::test]
async fn generated_update_roundtrips_omittable_unit_enum_input(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
type Status
    = Running
    | Stopped

record Game {
    @public
    id Id.Int @id
    status Status?
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch("insert into games (id, status) values (1, 'Running');")
        .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
query ListGames {
    game {
        id
        status
    }
}
"#,
        true,
    )?;
    let update = query_by_operation(&manifest, "update");
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;

    query::run(&conn, &manifest, &update.id, json!({ "id": 1 }), &session).await?;
    let mut rows = conn
        .query("select status from games where id = 1", ())
        .await?;
    assert_eq!(
        rows.next().await?.expect("game exists").get::<String>(0)?,
        "Running"
    );

    query::run(
        &conn,
        &manifest,
        &update.id,
        json!({ "id": 1, "status": { "_type": "Stopped" } }),
        &session,
    )
    .await?;
    let mut rows = conn
        .query("select status from games where id = 1", ())
        .await?;
    assert_eq!(
        rows.next().await?.expect("game exists").get::<String>(0)?,
        "Stopped"
    );

    query::run(
        &conn,
        &manifest,
        &update.id,
        json!({ "id": 1, "status": null }),
        &session,
    )
    .await?;
    let mut rows = conn
        .query("select status from games where id = 1", ())
        .await?;
    assert!(rows
        .next()
        .await?
        .expect("game exists")
        .get::<Option<String>>(0)?
        .is_none());

    Ok(())
}

#[tokio::test]
async fn run_mutation_roundtrips_recursive_union_json_payloads(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
type Action
    = Create { title String }
    | Delete { id Int }

type Tree
    = Leaf { action Action }
    | Branch { children Json<List<Tree>>, actions Json<Dict<Action>> }

record TreeDocument {
    @public
    id Id.Int @id
    tree Tree
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
insert CreateTreeDocument($tree: Tree) {
    treeDocument {
        tree = $tree
    }
}
"#,
        false,
    )?;
    let tree = json!({
        "_type": "Branch",
        "children": [{
            "_type": "Leaf",
            "action": { "_type": "Create", "title": "child" }
        }],
        "actions": {
            "deleted": { "_type": "Delete", "id": 7 }
        }
    });
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let result = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "tree": tree.clone() }),
        &session,
    )
    .await?;
    assert_eq!(result.response["treeDocument"][0]["tree"], tree);

    Ok(())
}

#[tokio::test]
async fn run_select_query_formats_response() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Note {
    id Int @id
    body String
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch("insert into notes (id, body, updatedAt) values (1, 'one', 10);")
        .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
query GetNotes {
    note {
        id
        body
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let result = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({}),
        &session,
    )
    .await?;

    assert_eq!(result.response["note"][0]["id"], json!(1));
    assert_eq!(result.response["note"][0]["body"], json!("one"));
    assert!(result.affected_rows.is_empty());

    Ok(())
}

#[tokio::test]
async fn run_insert_mutation_extracts_affected_rows_in_sync_mode(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Note {
    id Int @id
    body String
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
insert CreateNote($body: String) {
    note {
        body = $body
        updatedAt = 10
        id
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let result = query::run_sync(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "body": "one" }),
        &session,
    )
    .await?;

    assert_eq!(result.response, json!({}));
    assert_eq!(result.affected_rows.len(), 1);
    assert_eq!(result.affected_rows[0].table_name, "notes");
    assert_eq!(result.affected_rows[0].rows.len(), 1);

    Ok(())
}

#[tokio::test]
async fn run_query_applies_json_and_session_args() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
session {
    userId Int
}

record Note {
    id Int @id
    ownerId Int
    attrs Json
    updatedAt Int
    @allow(*) { ownerId == Session.userId }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
insert CreateNote($attrs: Json) {
    note {
        ownerId = Session.userId
        attrs = $attrs
        updatedAt = 10
        id
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({ "userId": 7 }), &manifest.session_schema)?;
    let result = query::run_sync(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "attrs": { "theme": "forest" } }),
        &session,
    )
    .await?;

    assert_eq!(result.affected_rows[0].rows.len(), 1);
    let mut rows = conn
        .query("select json(attrs) from notes where ownerId = 7", ())
        .await?;
    let row = rows.next().await?.expect("inserted row should exist");
    let attrs = serde_json::from_str::<serde_json::Value>(&row.get::<String>(0)?)?;
    assert_eq!(attrs, json!({ "theme": "forest" }));

    Ok(())
}

#[tokio::test]
async fn generated_update_respects_omitted_vs_null_optional_args(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Note {
    id Int @id
    body String?
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch("insert into notes (id, body, updatedAt) values (1, 'old', 10);")
        .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
query GetNotes {
    note {
        id
        body
    }
}
"#,
        true,
    )?;
    let update = query_by_operation(&manifest, "update");
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;

    query::run(&conn, &manifest, &update.id, json!({ "id": 1 }), &session).await?;
    let after_omitted = query::run(
        &conn,
        &manifest,
        &query_by_operation(&manifest, "query").id,
        json!({}),
        &session,
    )
    .await?;
    assert_eq!(after_omitted.response["note"][0]["body"], json!("old"));

    query::run(
        &conn,
        &manifest,
        &update.id,
        json!({ "id": 1, "body": null }),
        &session,
    )
    .await?;
    let after_null = query::run(
        &conn,
        &manifest,
        &query_by_operation(&manifest, "query").id,
        json!({}),
        &session,
    )
    .await?;
    assert_eq!(after_null.response["note"][0]["body"], json!(null));

    Ok(())
}

#[tokio::test]
async fn run_query_reports_unknown_and_invalid_input() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Note {
    id Int @id
    body String
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
query GetNote($id: Int) {
    note {
        @where { id == $id }
        id
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;

    let unknown = query::run(&conn, &manifest, "missing", json!({}), &session)
        .await
        .expect_err("unknown query should fail");
    assert_eq!(unknown.to_string(), "unknown query: missing");

    let invalid = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "id": "nope" }),
        &session,
    )
    .await
    .expect_err("invalid input should fail");
    assert_eq!(
        invalid.to_string(),
        "invalid input: input field 'id' must be Int"
    );

    Ok(())
}

#[tokio::test]
async fn run_query_reports_invalid_session() -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
session {
    userId Int
}

record Note {
    id Int @id
    ownerId Int
    body String
    updatedAt Int
    @allow(query) { ownerId == Session.userId }
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
query GetNotes {
    note {
        id
        body
    }
}
"#,
        false,
    )?;
    let empty_schema = Default::default();
    let session = PyreSession::new(json!({}), &empty_schema)?;

    let err = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({}),
        &session,
    )
    .await
    .expect_err("missing session arg should fail");

    assert_eq!(
        err.to_string(),
        "invalid session: missing session field 'userId'"
    );

    Ok(())
}

#[tokio::test]
async fn run_query_handles_parameter_names_with_shared_prefixes(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Note {
    id Int @id
    id2 Int
    body String
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch("insert into notes (id, id2, body, updatedAt) values (1, 2, 'one', 10);")
        .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
query GetNote($id: Int, $id2: Int) {
    note {
        @where { id == $id && id2 == $id2 }
        id
        id2
        body
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;

    let result = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "id": 1, "id2": 2 }),
        &session,
    )
    .await?;

    assert_eq!(result.response["note"][0]["body"], json!("one"));

    Ok(())
}

#[tokio::test]
async fn run_delete_mutation_extracts_affected_rows_in_sync_mode(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Note {
    id Int @id
    body String
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch("insert into notes (id, body, updatedAt) values (1, 'one', 10);")
        .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
delete DeleteNote($id: Int) {
    note {
        @where { id == $id }
        id
        body
        updatedAt
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let result = query::run_sync(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "id": 1 }),
        &session,
    )
    .await?;

    assert_eq!(result.affected_rows.len(), 1);
    assert_eq!(result.affected_rows[0].table_name, "notes");
    assert_eq!(result.affected_rows[0].rows[0][0], json!(1));
    let mut rows = conn.query("select count(*) from notes", ()).await?;
    let row = rows.next().await?.expect("count row should exist");
    assert_eq!(row.get::<i64>(0)?, 0);

    Ok(())
}

#[tokio::test]
async fn generated_crud_create_and_delete_run_through_manifest_runtime(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record Note {
    id Int @id
    body String
    updatedAt DateTime @default(now)
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
query GetNotes {
    note {
        id
        body
    }
}
"#,
        true,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let create = query_by_operation(&manifest, "insert");
    let delete = query_by_operation(&manifest, "delete");

    let created = query::run(
        &conn,
        &manifest,
        &create.id,
        json!({ "body": "generated" }),
        &session,
    )
    .await?;
    assert_eq!(created.response["note"][0]["body"], json!("generated"));
    assert!(created.affected_rows.is_empty());

    let deleted = query::run_sync(
        &conn,
        &manifest,
        &delete.id,
        json!({ "id": created.response["note"][0]["id"] }),
        &session,
    )
    .await?;
    assert_eq!(deleted.affected_rows.len(), 1);
    let remaining = query::run(
        &conn,
        &manifest,
        &query_by_operation(&manifest, "query").id,
        json!({}),
        &session,
    )
    .await?;
    assert_eq!(remaining.response["note"], json!([]));

    Ok(())
}

#[tokio::test]
async fn run_insert_mutation_binds_repeated_json_union_parameter(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
type Visibility
   = Hidden
   | Everyone
   | Users {
        userId Int
     }

record Scene {
    id Int @id
    visibility Visibility
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    let manifest = manifest_for(
        &db.context,
        r#"
insert CreateScene($visibility: Visibility) {
    scene {
        visibility = $visibility
        updatedAt = 10
        id
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;

    let result = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({ "visibility": { "_type": "Hidden" } }),
        &session,
    )
    .await?;

    assert_eq!(result.response["scene"][0]["id"], json!(1));

    let mut rows = conn.query("select visibility from scenes", ()).await?;
    let row = rows.next().await?.expect("scene row should exist");
    assert_eq!(row.get::<String>(0)?, "Hidden");

    Ok(())
}

#[tokio::test]
async fn run_multi_top_level_query_formats_all_response_keys(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = TestDatabase::new(
        r#"
record User {
    id Int @id
    name String
    updatedAt Int
    @public
}

record Note {
    id Int @id
    body String
    updatedAt Int
    @public
}
"#,
    )
    .await?;
    let conn = db.db.connect()?;
    conn.execute_batch(
        "insert into users (id, name, updatedAt) values (1, 'Ada', 10); insert into notes (id, body, updatedAt) values (1, 'one', 10);",
    )
    .await?;
    let manifest = manifest_for(
        &db.context,
        r#"
query Dashboard {
    user {
        id
        name
    }
    note {
        id
        body
    }
}
"#,
        false,
    )?;
    let session = PyreSession::new(json!({}), &manifest.session_schema)?;
    let result = query::run(
        &conn,
        &manifest,
        &only_query(&manifest).id,
        json!({}),
        &session,
    )
    .await?;

    assert_eq!(result.response["user"][0]["name"], json!("Ada"));
    assert_eq!(result.response["note"][0]["body"], json!("one"));

    Ok(())
}

#[cfg(feature = "filesystem")]
#[test]
fn manifest_load_reads_generated_manifest_file() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = Manifest {
        version: 1,
        session_schema: Default::default(),
        queries: Default::default(),
    };
    let dir = tempfile::TempDir::new()?;
    let path = dir.path().join("manifest.json");
    std::fs::write(&path, serde_json::to_string(&manifest)?)?;

    let loaded = Manifest::load(&path)?;

    assert_eq!(loaded.version, 1);
    assert!(loaded.queries.is_empty());

    Ok(())
}
