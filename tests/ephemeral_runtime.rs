use pyre::server::runtime::{
    DatabaseRuntime, Delivery, JoinEvidence, RuntimeConfig, RuntimeEnvironment, RuntimeError,
    RuntimeTime, SharedWritePolicy,
};
use pyre::{ast, ephemeral::Contract, parser, typecheck};
use serde_json::json;
use std::{
    sync::{
        atomic::{AtomicI64, AtomicU64, Ordering},
        Arc, Barrier,
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
    assert!(first
        .snapshot
        .connections
        .contains_key(first.participant.connection_id()));
    assert_eq!(second.snapshot.revision, first.snapshot.revision + 1);
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
    let expiring = runtime.join(evidence("expiring", 3, true)).unwrap();

    let left_change = runtime.leave(&left).unwrap().unwrap();
    assert_eq!(left_change.removed_connections, [left.connection_id()]);
    let transport_change = runtime.transport_closed(&transport).unwrap().unwrap();
    assert_eq!(
        transport_change.removed_connections,
        [transport.connection_id()]
    );
    environment.advance(9_000);
    runtime.renew(&expiring.participant, "expiring").unwrap();
    environment.advance(2_000);
    assert!(runtime.expire_leases().unwrap().is_none());
    environment.advance(8_000);
    assert_eq!(
        runtime.renew(&expiring.participant, "expiring"),
        Err(RuntimeError::LeaseExpired)
    );
    let expired = runtime.expire_leases().unwrap().unwrap();
    assert_eq!(
        expired.removed_connections,
        [expiring.participant.connection_id()]
    );
    assert_eq!(
        runtime.poll(&expiring.subscription),
        Err(RuntimeError::UnknownSubscription)
    );
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
fn shared_only_subscribers_enforce_write_intent_owner_policy_and_lease() {
    let environment = Arc::new(Environment::default());
    let contract = contract_with_states(
        r#"state Shared {
    count Int @default(0)
}
"#,
    );
    let server_only = DatabaseRuntime::new(
        "server-only",
        (),
        contract.clone(),
        config(environment.clone()),
    )
    .unwrap();
    let server_only_writer = server_only.subscribe("writer", true).unwrap();
    assert_eq!(
        server_only.patch_shared_from_subscription(
            &server_only_writer.subscription,
            "writer",
            &json!({"count": 1}),
        ),
        Err(RuntimeError::SharedServerOnly)
    );
    let mut runtime_config = config(environment.clone());
    runtime_config.downstream_delivery_cadence = Duration::ZERO;
    runtime_config.shared_write_policy = SharedWritePolicy::ParticipantWritable;
    let runtime = DatabaseRuntime::new("db", (), contract, runtime_config).unwrap();
    let subscribed = runtime.subscribe("reader", false).unwrap();
    let writer = runtime.subscribe("writer", true).unwrap();

    assert!(subscribed.snapshot.connections.is_empty());
    assert_eq!(subscribed.snapshot.shared.as_ref().unwrap()["count"], 0);
    assert!(!subscribed.subscription.is_writable());
    assert!(writer.subscription.is_writable());
    assert_eq!(
        runtime.patch_shared_from_subscription(
            &subscribed.subscription,
            "reader",
            &json!({"count": 3}),
        ),
        Err(RuntimeError::ReadOnly)
    );
    assert_eq!(
        runtime.patch_shared_from_subscription(
            &writer.subscription,
            "reader",
            &json!({"count": 3}),
        ),
        Err(RuntimeError::OwnerMismatch)
    );
    runtime
        .patch_shared_from_subscription(&writer.subscription, "writer", &json!({"count": 4}))
        .unwrap();
    let changed = delivery_change(runtime.poll(&subscribed.subscription).unwrap().unwrap());
    assert_eq!(changed.shared.unwrap()["count"], 4);
    assert_eq!(
        runtime.resubscribe(&subscribed.subscription, "forged"),
        Err(RuntimeError::OwnerMismatch)
    );
    let refreshed = runtime
        .resubscribe(&subscribed.subscription, "reader")
        .unwrap();
    assert_eq!(refreshed.snapshot.shared.unwrap()["count"], 4);
    assert!(!refreshed.subscription.is_writable());
    environment.advance(9_000);
    runtime
        .renew_subscription(&refreshed.subscription, "reader")
        .unwrap();
    environment.advance(10_000);
    assert_eq!(
        runtime.renew_subscription(&refreshed.subscription, "forged"),
        Err(RuntimeError::OwnerMismatch)
    );
    assert_eq!(
        runtime.patch_shared_from_subscription(
            &writer.subscription,
            "writer",
            &json!({"count": 5}),
        ),
        Err(RuntimeError::LeaseExpired)
    );
    assert!(runtime.expire_leases().unwrap().is_none());
    assert_eq!(
        runtime.poll(&refreshed.subscription),
        Err(RuntimeError::UnknownSubscription)
    );
    assert_eq!(
        runtime.patch_shared_from_subscription(
            &writer.subscription,
            "writer",
            &json!({"count": 5}),
        ),
        Err(RuntimeError::UnknownSubscription)
    );
}

#[test]
fn refresh_and_renew_is_atomic_and_owner_mismatch_does_not_renew() {
    let environment = Arc::new(Environment::default());
    let runtime = DatabaseRuntime::new("db", (), contract(), config(environment.clone())).unwrap();
    let participant = runtime
        .join(evidence("owner", 1, true))
        .unwrap()
        .participant;
    runtime
        .patch_connection(&participant, "owner", &json!({"cursor": "kept"}))
        .unwrap();
    environment.advance(9_000);

    assert_eq!(
        runtime.refresh_and_renew(
            &participant,
            "forged",
            &json!({"userId": 2, "role": "admin"}),
        ),
        Err(RuntimeError::OwnerMismatch)
    );
    environment.advance(1_000);
    assert_eq!(
        runtime.refresh_and_renew(
            &participant,
            "owner",
            &json!({"userId": 2, "role": "admin"}),
        ),
        Err(RuntimeError::LeaseExpired)
    );

    let active = runtime
        .join(evidence("active", 3, true))
        .unwrap()
        .participant;
    let (change, _) = runtime
        .refresh_and_renew(&active, "active", &json!({"userId": 4, "role": "admin"}))
        .unwrap();
    assert_eq!(
        change.unwrap().connections[active.connection_id()]["userId"],
        4
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

fn delivery_change(delivery: Delivery) -> pyre::server::runtime::Change {
    match delivery {
        Delivery::Changes { change } => change,
        other => panic!("expected changes delivery, got {other:?}"),
    }
}

#[test]
fn subscription_snapshot_has_no_concurrent_update_gap() {
    let environment = Arc::new(Environment::default());
    let mut runtime_config = config(environment.clone());
    runtime_config.downstream_delivery_cadence = Duration::ZERO;
    let runtime = Arc::new(DatabaseRuntime::new("db", (), contract(), runtime_config).unwrap());
    let barrier = Arc::new(Barrier::new(3));

    let joining = {
        let runtime = runtime.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            runtime.join(evidence("joining", 1, true)).unwrap()
        })
    };
    let updating = {
        let runtime = runtime.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            runtime.patch_shared(&json!({"count": 1})).unwrap();
        })
    };
    barrier.wait();
    let joined = joining.join().unwrap();
    updating.join().unwrap();

    let current = runtime.snapshot().unwrap();
    let mut reconstructed = joined.snapshot.clone();
    if let Some(delivery) = runtime.poll(&joined.subscription).unwrap() {
        let change = delivery_change(delivery);
        reconstructed.revision = change.revision;
        if change.shared.is_some() {
            reconstructed.shared = change.shared;
        }
        reconstructed.connections.extend(change.connections);
        for id in change.removed_connections {
            reconstructed.connections.remove(&id);
        }
    }
    assert_eq!(reconstructed, current);
}

#[test]
fn coalesces_complete_entries_and_delivers_on_trailing_cadence() {
    let environment = Arc::new(Environment::default());
    let mut runtime_config = config(environment.clone());
    runtime_config.downstream_delivery_cadence = Duration::from_millis(10);
    let runtime = DatabaseRuntime::new("db", (), contract(), runtime_config).unwrap();
    let observer = runtime.join(evidence("observer", 1, true)).unwrap();
    let actor = runtime.join(evidence("actor", 2, true)).unwrap();

    assert_eq!(runtime.poll(&observer.subscription).unwrap(), None);
    environment.advance(10);
    let joined = delivery_change(runtime.poll(&observer.subscription).unwrap().unwrap());
    assert_eq!(
        joined.connections[actor.participant.connection_id()]["color"],
        "blue"
    );

    runtime
        .patch_connection(&actor.participant, "actor", &json!({"cursor": "first"}))
        .unwrap();
    runtime
        .patch_connection(
            &actor.participant,
            "actor",
            &json!({"cursor": "last", "color": "red"}),
        )
        .unwrap();
    runtime.patch_shared(&json!({"count": 3})).unwrap();
    runtime.patch_shared(&json!({"label": "settled"})).unwrap();

    assert_eq!(runtime.poll(&observer.subscription).unwrap(), None);
    environment.advance(9);
    assert_eq!(runtime.poll(&observer.subscription).unwrap(), None);
    environment.advance(1);
    let change = delivery_change(runtime.poll(&observer.subscription).unwrap().unwrap());
    let connection = &change.connections[actor.participant.connection_id()];
    assert_eq!(connection["userId"], 2);
    assert_eq!(connection["cursor"], "last");
    assert_eq!(connection["color"], "red");
    let shared = change.shared.unwrap();
    assert_eq!(shared["count"], 3);
    assert_eq!(shared["label"], "settled");
    assert_eq!(change.revision, runtime.snapshot().unwrap().revision);
    assert_eq!(runtime.poll(&observer.subscription).unwrap(), None);

    assert_eq!(
        runtime
            .patch_connection(&actor.participant, "actor", &json!({"cursor": "last"}),)
            .unwrap(),
        None
    );
    environment.advance(100);
    assert_eq!(runtime.poll(&observer.subscription).unwrap(), None);
}

#[test]
fn removals_dominate_older_values_and_a_later_rejoin_wins() {
    let environment = Arc::new(Environment::default());
    let mut runtime_config = config(environment.clone());
    runtime_config.downstream_delivery_cadence = Duration::from_millis(10);
    let runtime = DatabaseRuntime::new("db", (), contract(), runtime_config).unwrap();
    let observer = runtime.join(evidence("observer", 1, true)).unwrap();
    let actor = runtime.join(evidence("actor", 2, true)).unwrap();
    let actor_id = actor.participant.connection_id().to_string();
    runtime.leave(&actor.participant).unwrap();
    environment.advance(10);
    let removed = delivery_change(runtime.poll(&observer.subscription).unwrap().unwrap());
    assert_eq!(removed.removed_connections, [actor_id.as_str()]);
    assert!(removed.connections.is_empty());

    environment.ids.store(2, Ordering::SeqCst);
    let replacement = runtime.join(evidence("replacement", 9, true)).unwrap();
    assert_eq!(replacement.participant.connection_id(), actor_id);
    environment.advance(10);
    let rejoined = delivery_change(runtime.poll(&observer.subscription).unwrap().unwrap());
    assert!(rejoined.removed_connections.is_empty());
    assert_eq!(rejoined.connections[&actor_id]["userId"], 9);

    runtime.leave(&replacement.participant).unwrap();
    environment.ids.store(2, Ordering::SeqCst);
    let newest = runtime.join(evidence("newest", 10, true)).unwrap();
    assert_eq!(newest.participant.connection_id(), actor_id);
    environment.advance(10);
    let churn = delivery_change(runtime.poll(&observer.subscription).unwrap().unwrap());
    assert!(churn.removed_connections.is_empty());
    assert_eq!(churn.connections[&actor_id]["userId"], 10);
}

#[test]
fn entry_overflow_requests_immediate_resync_and_suppresses_deltas() {
    let environment = Arc::new(Environment::default());
    let mut runtime_config = config(environment.clone());
    runtime_config.downstream_delivery_cadence = Duration::from_secs(60);
    runtime_config.max_pending_entries = 1;
    let runtime = DatabaseRuntime::new("db", (), contract(), runtime_config).unwrap();
    let observer = runtime.join(evidence("observer", 1, true)).unwrap();
    let first = runtime.join(evidence("first", 2, true)).unwrap();
    runtime.join(evidence("second", 3, true)).unwrap();

    let recovery_revision = match runtime.poll(&observer.subscription).unwrap().unwrap() {
        Delivery::ResyncRequired { recovery } => recovery.revision,
        other => panic!("expected recovery delivery, got {other:?}"),
    };
    assert_eq!(recovery_revision, runtime.snapshot().unwrap().revision);
    runtime
        .patch_connection(&first.participant, "first", &json!({"cursor": "later"}))
        .unwrap();
    assert_eq!(runtime.poll(&observer.subscription).unwrap(), None);

    let refreshed = runtime
        .resubscribe(&observer.subscription, "observer")
        .unwrap();
    assert_eq!(
        runtime.poll(&observer.subscription),
        Err(RuntimeError::StaleSubscription)
    );
    assert_eq!(refreshed.snapshot, runtime.snapshot().unwrap());
    runtime
        .patch_connection(&first.participant, "first", &json!({"cursor": "latest"}))
        .unwrap();
    assert_eq!(runtime.poll(&refreshed.subscription).unwrap(), None);
    environment.advance(60_000);
    let change = delivery_change(runtime.poll(&refreshed.subscription).unwrap().unwrap());
    assert_eq!(
        change.connections[first.participant.connection_id()]["cursor"],
        "latest"
    );
}

#[test]
fn payload_and_transport_bounds_fail_explicitly() {
    let environment = Arc::new(Environment::default());
    let mut invalid = config(environment.clone());
    invalid.max_pending_controls = 0;
    assert!(matches!(
        DatabaseRuntime::new("db", (), contract(), invalid),
        Err(RuntimeError::InvalidTransportBounds)
    ));

    let mut snapshot_limited = config(environment.clone());
    snapshot_limited.max_delivery_bytes = 1;
    let runtime = DatabaseRuntime::new("tiny", (), contract(), snapshot_limited).unwrap();
    assert_eq!(runtime.snapshot(), Err(RuntimeError::PayloadTooLarge));
    assert!(matches!(
        runtime.join(evidence("owner", 1, true)),
        Err(RuntimeError::PayloadTooLarge)
    ));

    let mut delivery_limited = config(environment);
    delivery_limited.downstream_delivery_cadence = Duration::from_secs(60);
    delivery_limited.max_delivery_bytes = 1024;
    let runtime = DatabaseRuntime::new("bounded", (), contract(), delivery_limited).unwrap();
    let observer = runtime.join(evidence("observer", 1, true)).unwrap();
    runtime
        .patch_shared(&json!({"label": "x".repeat(5_000)}))
        .unwrap();
    assert!(matches!(
        runtime.poll(&observer.subscription).unwrap(),
        Some(Delivery::ResyncRequired { .. })
    ));
    assert_eq!(
        runtime.resubscribe(&observer.subscription, "observer"),
        Err(RuntimeError::PayloadTooLarge)
    );
}

#[test]
fn subscription_cleanup_stale_handles_and_database_isolation() {
    let environment = Arc::new(Environment::default());
    let mut runtime_config = config(environment.clone());
    runtime_config.downstream_delivery_cadence = Duration::ZERO;
    let left = DatabaseRuntime::new("left", (), contract(), runtime_config.clone()).unwrap();
    let right = DatabaseRuntime::new("right", (), contract(), runtime_config).unwrap();
    let observer = left.join(evidence("observer", 1, true)).unwrap();
    let leaving = left.join(evidence("leaving", 2, true)).unwrap();

    left.transport_closed(&leaving.participant).unwrap();
    assert_eq!(
        left.poll(&leaving.subscription),
        Err(RuntimeError::UnknownSubscription)
    );
    let removal = delivery_change(left.poll(&observer.subscription).unwrap().unwrap());
    assert_eq!(
        removal.removed_connections,
        [leaving.participant.connection_id()]
    );
    assert_eq!(
        right.poll(&observer.subscription),
        Err(RuntimeError::WrongDatabase)
    );

    left.authorization_lost(&observer.participant).unwrap();
    assert_eq!(
        left.poll(&observer.subscription),
        Err(RuntimeError::UnknownSubscription)
    );

    let closing = right.join(evidence("closing", 3, true)).unwrap();
    right.close().unwrap();
    assert_eq!(right.poll(&closing.subscription), Err(RuntimeError::Closed));

    let reopened = DatabaseRuntime::new("left", (), contract(), config(environment)).unwrap();
    assert_eq!(
        reopened.poll(&observer.subscription),
        Err(RuntimeError::StaleEpoch)
    );
}

#[test]
fn reconnect_has_a_fresh_identity_snapshot_and_no_replay() {
    let environment = Arc::new(Environment::default());
    let mut runtime_config = config(environment);
    runtime_config.downstream_delivery_cadence = Duration::ZERO;
    let runtime = DatabaseRuntime::new("db", (), contract(), runtime_config).unwrap();
    let first = runtime.join(evidence("owner", 1, true)).unwrap();
    runtime.patch_shared(&json!({"count": 7})).unwrap();
    runtime.transport_closed(&first.participant).unwrap();

    let reconnected = runtime.join(evidence("owner", 1, true)).unwrap();
    assert_ne!(
        reconnected.participant.connection_id(),
        first.participant.connection_id()
    );
    assert_eq!(reconnected.snapshot.shared.as_ref().unwrap()["count"], 7);
    assert!(reconnected
        .snapshot
        .connections
        .contains_key(reconnected.participant.connection_id()));
    assert_eq!(runtime.poll(&reconnected.subscription).unwrap(), None);
}

#[test]
fn delivery_envelopes_serialize_with_ephemeral_identity() {
    let environment = Arc::new(Environment::default());
    let mut runtime_config = config(environment.clone());
    runtime_config.downstream_delivery_cadence = Duration::ZERO;
    let runtime = DatabaseRuntime::new("db", (), contract(), runtime_config).unwrap();
    let observer = runtime.join(evidence("observer", 1, true)).unwrap();
    runtime.patch_shared(&json!({"count": 4})).unwrap();
    let delivery = runtime.poll(&observer.subscription).unwrap().unwrap();
    let encoded = serde_json::to_value(delivery).unwrap();
    assert_eq!(encoded["type"], "ephemeralChanges");
    assert_eq!(encoded["ephemeralChanges"]["databaseId"], "db");
    assert_eq!(encoded["ephemeralChanges"]["epoch"], runtime.epoch());
    assert!(encoded["ephemeralChanges"]["revision"].as_u64().is_some());
    assert!(encoded.get("cursor").is_none());
}
