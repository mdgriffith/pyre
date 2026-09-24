use pyre::server::runtime::{
    DatabaseRuntime, JoinEvidence, RuntimeConfig, RuntimeEnvironment, RuntimeError, RuntimeTime,
    SharedWritePolicy,
};
use pyre::{ast, ephemeral::Contract, parser, typecheck};
use serde_json::json;
use std::{
    sync::{
        atomic::{AtomicI64, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

#[derive(Default)]
struct Environment {
    millis: AtomicU64,
    seconds: AtomicI64,
    ids: AtomicU64,
}

impl Environment {
    fn advance(&self, millis: u64) {
        self.millis.fetch_add(millis, Ordering::SeqCst);
        self.seconds
            .fetch_add((millis / 1_000) as i64, Ordering::SeqCst);
    }
}

impl RuntimeEnvironment for Environment {
    fn now(&self) -> RuntimeTime {
        RuntimeTime {
            monotonic_millis: self.millis.load(Ordering::SeqCst),
            unix_seconds: self.seconds.load(Ordering::SeqCst),
        }
    }

    fn new_id(&self) -> Result<String, String> {
        Ok(format!(
            "opaque-{}",
            self.ids.fetch_add(1, Ordering::SeqCst)
        ))
    }
}

fn contract() -> Contract {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        r#"session {
    userId Int
    role String
}

state Connection {
    userId Int = Session.userId
    role String = Session.role
    cursor String?
    color String @default("blue")
}

state Shared {
    count Int @default(0)
    label String @default("new")
    opened DateTime @default(now)
}
"#,
        &mut schema,
    )
    .unwrap();
    let context = typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .unwrap();
    Contract::from_context(&context).unwrap()
}

fn contract_with_states(source: &str) -> Contract {
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", source, &mut schema).unwrap();
    let context = typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .unwrap();
    Contract::from_context(&context).unwrap()
}

fn config(environment: Arc<Environment>) -> RuntimeConfig {
    RuntimeConfig {
        lease_duration: Duration::from_secs(10),
        environment,
        ..RuntimeConfig::default()
    }
}

fn evidence(owner: &str, user_id: i64, writable: bool) -> JoinEvidence {
    JoinEvidence {
        owner_id: owner.to_string(),
        trusted_session: json!({"userId": user_id, "role": "member"}),
        writable,
    }
}

#[test]
fn runtimes_isolate_databases_and_same_owner_connections() {
    let environment = Arc::new(Environment::default());
    let left = DatabaseRuntime::new("left", 11, contract(), config(environment.clone())).unwrap();
    let right = DatabaseRuntime::new("right", 22, contract(), config(environment)).unwrap();

    let first = left.join(evidence("session-a", 1, true)).unwrap();
    let second = left.join(evidence("session-a", 1, true)).unwrap();
    let other = right.join(evidence("session-a", 1, true)).unwrap();
    assert_eq!(first.snapshot.revision, first.change.revision);
    assert!(first
        .snapshot
        .connections
        .contains_key(first.participant.connection_id()));
    assert_eq!(second.snapshot.revision, second.change.revision);
    assert_eq!(second.change.revision, first.change.revision + 1);
    assert_ne!(
        first.participant.connection_id(),
        second.participant.connection_id()
    );
    assert_ne!(left.epoch(), right.epoch());
    assert_eq!(left.database(), &11);
    assert_eq!(right.database(), &22);
    assert_eq!(left.snapshot().unwrap().connections.len(), 2);
    assert_eq!(right.snapshot().unwrap().connections.len(), 1);

    left.patch_connection(&first.participant, "session-a", &json!({"cursor": "left"}))
        .unwrap();
    assert_eq!(right.snapshot().unwrap().connections.len(), 1);
    assert_eq!(other.snapshot.connections.len(), 1);
}

#[test]
fn enforces_owner_read_only_and_shared_policy_without_mutation() {
    let environment = Arc::new(Environment::default());
    let runtime = DatabaseRuntime::new("db", (), contract(), config(environment)).unwrap();
    let writer = runtime.join(evidence("writer", 1, true)).unwrap();
    let reader = runtime.join(evidence("reader", 2, false)).unwrap();
    let revision = runtime.snapshot().unwrap().revision;

    assert_eq!(
        runtime.patch_connection(&writer.participant, "forged", &json!({"cursor": "x"})),
        Err(RuntimeError::OwnerMismatch)
    );
    assert_eq!(
        runtime.patch_connection(&reader.participant, "reader", &json!({"cursor": "x"})),
        Err(RuntimeError::ReadOnly)
    );
    assert_eq!(
        runtime.patch_shared_from_participant(&writer.participant, "writer", &json!({"count": 1}),),
        Err(RuntimeError::SharedServerOnly)
    );
    assert_eq!(runtime.snapshot().unwrap().revision, revision);

    let changed = runtime.patch_shared(&json!({"count": 1})).unwrap().unwrap();
    assert_eq!(changed.shared.unwrap()["count"], 1);
    assert_eq!(changed.revision, revision + 1);
}

#[test]
fn participant_shared_writes_require_both_policy_and_write_access() {
    let environment = Arc::new(Environment::default());
    let mut runtime_config = config(environment);
    runtime_config.shared_write_policy = SharedWritePolicy::ParticipantWritable;
    let runtime = DatabaseRuntime::new("db", (), contract(), runtime_config).unwrap();
    let reader = runtime.join(evidence("reader", 1, false)).unwrap();
    let writer = runtime.join(evidence("writer", 2, true)).unwrap();
    assert_eq!(
        runtime.patch_shared_from_participant(&reader.participant, "reader", &json!({"count": 1}),),
        Err(RuntimeError::ReadOnly)
    );
    runtime
        .patch_shared_from_participant(&writer.participant, "writer", &json!({"count": 2}))
        .unwrap();
    assert_eq!(runtime.snapshot().unwrap().shared.unwrap()["count"], 2);
}

#[test]
fn invalid_and_noop_patches_are_atomic_and_do_not_advance_revision() {
    let environment = Arc::new(Environment::default());
    let runtime = DatabaseRuntime::new("db", (), contract(), config(environment)).unwrap();
    let participant = runtime
        .join(evidence("owner", 1, true))
        .unwrap()
        .participant;
    let before = runtime.snapshot().unwrap();

    assert!(matches!(
        runtime.patch_connection(&participant, "owner", &json!({"cursor": "ok", "color": 4}),),
        Err(RuntimeError::Validation(_))
    ));
    assert_eq!(runtime.snapshot().unwrap(), before);
    assert_eq!(
        runtime
            .patch_connection(&participant, "owner", &json!({"color": "blue"}))
            .unwrap(),
        None
    );
    assert_eq!(runtime.snapshot().unwrap(), before);
    assert_eq!(runtime.patch_shared(&json!({"count": 0})).unwrap(), None);
    assert_eq!(runtime.snapshot().unwrap(), before);
}

#[test]
fn refresh_recomputes_derived_fields_and_preserves_writable_fields() {
    let environment = Arc::new(Environment::default());
    let runtime = DatabaseRuntime::new("db", (), contract(), config(environment)).unwrap();
    let participant = runtime
        .join(evidence("owner", 1, true))
        .unwrap()
        .participant;
    runtime
        .patch_connection(
            &participant,
            "owner",
            &json!({"cursor": "position", "color": "red"}),
        )
        .unwrap();
    let change = runtime
        .refresh_connection(
            &participant,
            "owner",
            &json!({"userId": 9, "role": "admin"}),
        )
        .unwrap()
        .unwrap();
    let value = &change.connections[participant.connection_id()];
    assert_eq!(value["userId"], 9);
    assert_eq!(value["role"], "admin");
    assert_eq!(value["cursor"], "position");
    assert_eq!(value["color"], "red");
}

#[test]
fn leave_transport_close_expiry_and_renewal_publish_removals() {
    let environment = Arc::new(Environment::default());
    let runtime = DatabaseRuntime::new("db", (), contract(), config(environment.clone())).unwrap();
    let left = runtime.join(evidence("left", 1, true)).unwrap().participant;
    let transport = runtime
        .join(evidence("transport", 2, true))
        .unwrap()
        .participant;
    let expiring = runtime
        .join(evidence("expiring", 3, true))
        .unwrap()
        .participant;

    let left_change = runtime.leave(&left).unwrap().unwrap();
    assert_eq!(left_change.removed_connections, [left.connection_id()]);
    let transport_change = runtime.transport_closed(&transport).unwrap().unwrap();
    assert_eq!(
        transport_change.removed_connections,
        [transport.connection_id()]
    );
    environment.advance(9_000);
    runtime.renew(&expiring, "expiring").unwrap();
    environment.advance(2_000);
    assert!(runtime.expire_leases().unwrap().is_none());
    environment.advance(8_000);
    assert_eq!(
        runtime.renew(&expiring, "expiring"),
        Err(RuntimeError::LeaseExpired)
    );
    let expired = runtime.expire_leases().unwrap().unwrap();
    assert_eq!(expired.removed_connections, [expiring.connection_id()]);
    assert!(runtime.snapshot().unwrap().connections.is_empty());
}

#[test]
fn participant_bound_is_checked_before_mutation_and_shared_survives_zero() {
    let environment = Arc::new(Environment::default());
    let mut runtime_config = config(environment.clone());
    runtime_config.max_participants = 1;
    let runtime = DatabaseRuntime::new("db", (), contract(), runtime_config).unwrap();
    runtime.patch_shared(&json!({"label": "resident"})).unwrap();
    let participant = runtime.join(evidence("one", 1, true)).unwrap().participant;
    let before = runtime.snapshot().unwrap();
    assert_eq!(
        runtime.join(evidence("two", 2, true)),
        Err(RuntimeError::Capacity)
    );
    assert_eq!(runtime.snapshot().unwrap(), before);
    runtime.leave(&participant).unwrap();
    let replacement = runtime.join(evidence("two", 2, true)).unwrap().participant;
    assert_eq!(replacement.connection_id(), "opaque-2");
    runtime.leave(&replacement).unwrap();
    let empty = runtime.snapshot().unwrap();
    assert!(empty.connections.is_empty());
    assert_eq!(empty.shared.unwrap()["label"], "resident");
}

#[test]
fn close_fences_handles_and_reopen_has_new_epoch_and_defaults() {
    let environment = Arc::new(Environment::default());
    let runtime = DatabaseRuntime::new("db", (), contract(), config(environment.clone())).unwrap();
    runtime.patch_shared(&json!({"count": 8})).unwrap();
    let participant = runtime
        .join(evidence("owner", 1, true))
        .unwrap()
        .participant;
    let old_epoch = runtime.epoch().to_string();
    let close = runtime.close().unwrap().unwrap();
    assert_eq!(close.removed_connections, [participant.connection_id()]);
    assert_eq!(runtime.snapshot(), Err(RuntimeError::Closed));
    assert_eq!(
        runtime.patch_connection(&participant, "owner", &json!({"cursor": "late"})),
        Err(RuntimeError::Closed)
    );

    let reopened = DatabaseRuntime::new("db", (), contract(), config(environment)).unwrap();
    assert_ne!(reopened.epoch(), old_epoch);
    assert_eq!(reopened.snapshot().unwrap().shared.unwrap()["count"], 0);
    assert_eq!(
        reopened.patch_connection(&participant, "", &json!({"cursor": "stale"})),
        Err(RuntimeError::StaleEpoch)
    );

    let other = DatabaseRuntime::new(
        "other",
        (),
        contract(),
        RuntimeConfig {
            environment: Arc::new(Environment::default()),
            ..RuntimeConfig::default()
        },
    )
    .unwrap();
    assert_eq!(
        other.patch_connection(&participant, "", &json!({"cursor": "wrong"})),
        Err(RuntimeError::WrongDatabase)
    );
}

#[test]
fn concurrent_shared_top_level_writes_are_ordered_and_converge() {
    let environment = Arc::new(Environment::default());
    let runtime =
        Arc::new(DatabaseRuntime::new("db", (), contract(), config(environment)).unwrap());
    let initial_revision = runtime.snapshot().unwrap().revision;
    let mut workers = Vec::new();
    for value in 1..=20 {
        let runtime = runtime.clone();
        workers.push(thread::spawn(move || {
            let patch = if value % 2 == 0 {
                json!({"count": value})
            } else {
                json!({"label": format!("label-{value}")})
            };
            runtime.patch_shared(&patch).unwrap().unwrap()
        }));
    }
    let mut revisions: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap().revision)
        .collect();
    revisions.sort_unstable();
    assert_eq!(
        revisions,
        ((initial_revision + 1)..=(initial_revision + 20)).collect::<Vec<_>>()
    );
    let snapshot = runtime.snapshot().unwrap();
    assert_eq!(snapshot.revision, initial_revision + 20);
    let shared = snapshot.shared.unwrap();
    assert!(shared["count"].as_i64().unwrap() > 0);
    assert!(shared["label"].as_str().unwrap().starts_with("label-"));
}

#[test]
fn connection_only_runtime_has_no_shared_value_and_rejects_shared_api() {
    let environment = Arc::new(Environment::default());
    let contract = contract_with_states(
        r#"state Connection {
    cursor String?
}
"#,
    );
    let runtime = DatabaseRuntime::new("db", (), contract, config(environment)).unwrap();
    assert_eq!(runtime.snapshot().unwrap().shared, None);

    let joined = runtime.join(evidence("owner", 1, true)).unwrap();
    assert_eq!(joined.snapshot.shared, None);
    assert_eq!(
        runtime.patch_shared(&json!({})),
        Err(RuntimeError::StateNotDeclared("Shared"))
    );
}

#[test]
fn shared_only_runtime_supports_shared_state_but_rejects_join() {
    let environment = Arc::new(Environment::default());
    let contract = contract_with_states(
        r#"state Shared {
    count Int @default(0)
}
"#,
    );
    let runtime = DatabaseRuntime::new("db", (), contract, config(environment)).unwrap();
    assert!(runtime.snapshot().unwrap().connections.is_empty());
    runtime.patch_shared(&json!({"count": 2})).unwrap();
    assert_eq!(runtime.snapshot().unwrap().shared.unwrap()["count"], 2);
    assert_eq!(
        runtime.join(evidence("owner", 1, true)),
        Err(RuntimeError::StateNotDeclared("Connection"))
    );
}

#[test]
fn rejects_empty_trusted_owner_evidence_without_mutation() {
    let environment = Arc::new(Environment::default());
    let runtime = DatabaseRuntime::new("db", (), contract(), config(environment)).unwrap();
    assert_eq!(
        runtime.join(evidence("  ", 1, true)),
        Err(RuntimeError::EmptyOwnerId)
    );
    assert_eq!(runtime.snapshot().unwrap().revision, 0);
    assert!(matches!(
        runtime.join(JoinEvidence {
            owner_id: "owner".to_string(),
            trusted_session: json!({}),
            writable: true,
        }),
        Err(RuntimeError::Validation(_))
    ));
    assert_eq!(runtime.snapshot().unwrap().revision, 0);

    let participant = runtime
        .join(evidence("owner", 1, true))
        .unwrap()
        .participant;
    let before = runtime.snapshot().unwrap();
    assert_eq!(
        runtime.patch_connection(&participant, "", &json!({"cursor": "x"})),
        Err(RuntimeError::EmptyOwnerId)
    );
    assert_eq!(runtime.snapshot().unwrap(), before);
}

#[test]
fn closed_runtime_is_distinct_from_undeclared_shared_state() {
    let environment = Arc::new(Environment::default());
    let contract = contract_with_states(
        r#"state Connection {
    cursor String?
}
"#,
    );
    let runtime = DatabaseRuntime::new("db", (), contract, config(environment)).unwrap();
    assert_eq!(
        runtime.patch_shared(&json!({})),
        Err(RuntimeError::StateNotDeclared("Shared"))
    );
    runtime.close().unwrap();
    assert_eq!(runtime.patch_shared(&json!({})), Err(RuntimeError::Closed));
    assert_eq!(runtime.snapshot(), Err(RuntimeError::Closed));
}

#[test]
fn authorization_loss_removes_the_connection() {
    let environment = Arc::new(Environment::default());
    let runtime = DatabaseRuntime::new("db", (), contract(), config(environment)).unwrap();
    let participant = runtime
        .join(evidence("owner", 1, true))
        .unwrap()
        .participant;
    let change = runtime.authorization_lost(&participant).unwrap().unwrap();
    assert_eq!(change.removed_connections, [participant.connection_id()]);
    assert_eq!(
        runtime.renew(&participant, "owner"),
        Err(RuntimeError::UnknownConnection)
    );
}

#[test]
fn generated_id_collisions_and_clock_overflow_do_not_mutate_state() {
    let environment = Arc::new(Environment::default());
    let runtime = DatabaseRuntime::new("db", (), contract(), config(environment.clone())).unwrap();
    runtime.join(evidence("first", 1, true)).unwrap();
    let before = runtime.snapshot().unwrap();

    environment.ids.store(1, Ordering::SeqCst);
    assert_eq!(
        runtime.join(evidence("second", 2, true)),
        Err(RuntimeError::IdCollision)
    );
    assert_eq!(runtime.snapshot().unwrap(), before);

    let overflowing = Arc::new(Environment::default());
    overflowing.millis.store(u64::MAX, Ordering::SeqCst);
    assert!(matches!(
        DatabaseRuntime::new("overflow", (), contract(), config(overflowing)),
        Err(RuntimeError::ClockOverflow)
    ));
}
