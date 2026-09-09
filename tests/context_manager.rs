#![cfg(all(feature = "database", feature = "json", not(target_arch = "wasm32")))]

use pyre::server::{
    context::{Config, ContextManager, Error, RunError, SessionFuture},
    manifest::{Manifest, PyreSession},
    query,
};
use std::{
    future::{poll_fn, Future},
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::Poll,
    time::Duration,
};

struct Login {
    id: String,
    resolves: AtomicUsize,
    gate: tokio::sync::Semaphore,
}
impl Login {
    fn new(id: &str, permits: usize) -> Self {
        Self {
            id: id.into(),
            resolves: AtomicUsize::new(0),
            gate: tokio::sync::Semaphore::new(permits),
        }
    }
}
fn key(login: &Login) -> String {
    login.id.clone()
}
fn resolve<'a>(login: &'a Login, db: &'a str) -> SessionFuture<'a, String> {
    Box::pin(async move {
        login.resolves.fetch_add(1, Ordering::SeqCst);
        login.gate.acquire().await.unwrap().forget();
        match (login.id.as_str(), db) {
            ("alice", "a") => Some("admin".into()),
            ("alice", "b") | ("bob", "a") => Some("reader".into()),
            _ => None,
        }
    })
}
async fn pending(mut future: Pin<&mut impl Future>) {
    poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
}

#[test]
fn pair_cache_roles_denial_and_invalidation() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let calls = AtomicUsize::new(0);
        let manager = ContextManager::new(Config {
            get_session_key: key,
            resolve_session: resolve,
            get_database: |db: String| {
                calls.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Ok::<_, i32>(db))
            },
            max_age: Duration::from_secs(60),
        })
        .unwrap();
        let alice = Login::new("alice", 20);
        let bob = Login::new("bob", 20);
        let a = manager.get(&alice, "a").await.unwrap();
        assert!(Arc::ptr_eq(&a, &manager.get(&alice, "a").await.unwrap()));
        assert_eq!(alice.resolves.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let b = manager.get(&alice, "b").await.unwrap();
        let other = manager.get(&bob, "a").await.unwrap();
        for (context, role, db) in [
            (&a, "admin", "a"),
            (&b, "reader", "b"),
            (&other, "reader", "a"),
        ] {
            let value = context
                .run(
                    |handle, session, input| async move {
                        Ok::<_, i32>((handle, (*session).clone(), input))
                    },
                    7,
                )
                .await
                .unwrap();
            assert_eq!(value, (db.to_owned(), role.to_owned(), 7));
        }
        assert!(matches!(manager.get(&bob, "b").await, Err(Error::Denied)));
        assert!(matches!(
            a.run(|_, _, _| async { Err::<(), _>(42) }, ()).await,
            Err(RunError::Operation(42))
        ));
        manager.invalidate_pair("alice", "a");
        assert!(!a.is_valid());
        assert!(b.is_valid() && other.is_valid());
        let replacement = manager.get(&alice, "a").await.unwrap();
        manager.invalidate_session("alice");
        assert!(!replacement.is_valid() && !b.is_valid() && other.is_valid());
        manager.invalidate_database("a");
        assert!(!other.is_valid());
        let retained = manager.get(&alice, "a").await.unwrap();
        manager.shutdown();
        assert!(!retained.is_valid());
        assert!(matches!(
            manager.get(&alice, "a").await,
            Err(Error::Shutdown)
        ));
        assert_eq!(alice.id, "alice");
    });
}

#[test]
fn pending_resolution_fences_duplicates_and_cancellation() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let manager = ContextManager::new(Config {
            get_session_key: key,
            resolve_session: resolve,
            get_database: |db: String| std::future::ready(Ok::<_, ()>(db)),
            max_age: Duration::from_secs(60),
        })
        .unwrap();
        let login = Login::new("alice", 0);
        let mut old = Box::pin(manager.get(&login, "a"));
        pending(old.as_mut()).await;
        manager.invalidate_database("a");
        login.gate.add_permits(2);
        assert!(matches!(old.await, Err(Error::Stale)));
        let fresh = manager.get(&login, "a").await.unwrap();
        manager.invalidate_session("alice");
        let mut first = Box::pin(manager.get(&login, "a"));
        let mut second = Box::pin(manager.get(&login, "a"));
        pending(first.as_mut()).await;
        pending(second.as_mut()).await;
        login.gate.add_permits(2);
        let first = first.await.unwrap();
        assert!(Arc::ptr_eq(&first, &second.await.unwrap()));
        assert!(!fresh.is_valid());
        manager.invalidate_pair("alice", "a");
        let mut futures = Vec::new();
        for _ in 0..128 {
            let mut future = Box::pin(manager.get(&login, "a"));
            pending(future.as_mut()).await;
            futures.push(future);
        }
        assert!(matches!(
            manager.get(&login, "a").await,
            Err(Error::Capacity)
        ));
        drop(futures);
        login.gate.add_permits(1);
        assert!(manager.get(&login, "a").await.is_ok());
    });
}

#[test]
fn expiry_and_manager_drop_invalidate_retained_contexts() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let manager = ContextManager::new(Config {
            get_session_key: key,
            resolve_session: resolve,
            get_database: |db: String| std::future::ready(Ok::<_, ()>(db)),
            max_age: Duration::from_millis(20),
        })
        .unwrap();
        let login = Login::new("alice", 3);
        let old = manager.get(&login, "a").await.unwrap();
        std::thread::sleep(Duration::from_millis(30));
        assert!(!old.is_valid());
        let new = manager.get(&login, "a").await.unwrap();
        assert!(!Arc::ptr_eq(&old, &new));
        drop(manager);
        assert!(!new.is_valid());
    });
}

#[test]
fn invalidation_during_database_and_operation_waits() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let gate = tokio::sync::Semaphore::new(0);
        let manager = ContextManager::new(Config {
            get_session_key: key,
            resolve_session: resolve,
            get_database: |_: String| async {
                gate.acquire().await.unwrap().forget();
                Ok::<_, i32>(())
            },
            max_age: Duration::from_secs(60),
        })
        .unwrap();
        let login = Login::new("alice", 3);
        let context = manager.get(&login, "a").await.unwrap();
        let mut run = Box::pin(context.run(
            |_, _, _| async {
                panic!("must not execute");
                #[allow(unreachable_code)]
                Ok::<_, i32>(())
            },
            (),
        ));
        pending(run.as_mut()).await;
        manager.invalidate_pair("alice", "a");
        gate.add_permits(1);
        assert!(matches!(run.await, Err(RunError::Stale)));
        for error in [false, true] {
            let context = manager.get(&login, "a").await.unwrap();
            gate.add_permits(1);
            let operation_gate = tokio::sync::Semaphore::new(0);
            let mut run = Box::pin(context.run(
                |_, _, _| async {
                    operation_gate.acquire().await.unwrap().forget();
                    if error {
                        Err(42)
                    } else {
                        Ok(())
                    }
                },
                (),
            ));
            pending(run.as_mut()).await;
            manager.invalidate_session("alice");
            operation_gate.add_permits(1);
            match run.await {
                Err(RunError::StaleExecution { operation_error }) => {
                    assert_eq!(operation_error, error.then_some(42))
                }
                result => panic!("unexpected {result:?}"),
            }
        }
    });
}

#[test]
fn failed_database_lookup_never_dispatches_and_invalidation_takes_precedence() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let gate = tokio::sync::Semaphore::new(1);
        let calls = AtomicUsize::new(0);
        let manager = ContextManager::new(Config {
            get_session_key: key,
            resolve_session: resolve,
            get_database: |_: String| async {
                gate.acquire().await.unwrap().forget();
                Err::<String, _>("lookup failed")
            },
            max_age: Duration::from_secs(60),
        })
        .unwrap();
        let login = Login::new("alice", 1);
        let context = manager.get(&login, "a").await.unwrap();
        let operation = |_: String, _: Arc<String>, ()| {
            calls.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok::<_, &str>(()))
        };
        assert!(matches!(
            context.run(operation, ()).await,
            Err(RunError::Database("lookup failed"))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(context.is_valid());

        let mut run = Box::pin(context.run(operation, ()));
        pending(run.as_mut()).await;
        manager.invalidate_pair("alice", "a");
        gate.add_permits(1);
        assert!(matches!(run.await, Err(RunError::Stale)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    });
}

struct NativeLogin {
    manifest: Arc<Manifest>,
    user_id: i64,
}

#[test]
fn bounded_cache_and_pending_expiry_and_shutdown() {
    fn allow<'a>(_: &'a Login, _: &'a str) -> SessionFuture<'a, ()> {
        Box::pin(std::future::ready(Some(())))
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let config = |max_age| Config {
            get_session_key: key,
            resolve_session: allow,
            get_database: |db: String| std::future::ready(Ok::<_, ()>(db)),
            max_age,
        };
        assert!(matches!(
            ContextManager::<(), _, _, _>::new(config(Duration::ZERO)),
            Err(Error::InvalidMaxAge)
        ));
        assert!(matches!(
            ContextManager::<(), _, _, _>::new(config(Duration::MAX)),
            Err(Error::InvalidMaxAge)
        ));
        let manager = ContextManager::new(config(Duration::from_secs(60))).unwrap();
        let login = Login::new("alice", 0);
        let retained = manager.get(&login, "0").await.unwrap();
        for i in 1..1024 {
            manager.get(&login, &i.to_string()).await.unwrap();
        }
        assert!(matches!(
            manager.get(&login, "overflow").await,
            Err(Error::Capacity)
        ));
        assert!(Arc::ptr_eq(
            &retained,
            &manager.get(&login, "0").await.unwrap()
        ));
        manager.invalidate_database("0");
        assert!(manager.get(&login, "overflow").await.is_ok());

        let manager = ContextManager::new(Config {
            get_session_key: key,
            resolve_session: resolve,
            get_database: |db: String| std::future::ready(Ok::<_, ()>(db)),
            max_age: Duration::from_millis(20),
        })
        .unwrap();
        let mut resolution = Box::pin(manager.get(&login, "a"));
        pending(resolution.as_mut()).await;
        std::thread::sleep(Duration::from_millis(30));
        login.gate.add_permits(1);
        assert!(matches!(resolution.await, Err(Error::Stale)));
        let mut resolution = Box::pin(manager.get(&login, "a"));
        pending(resolution.as_mut()).await;
        manager.shutdown();
        login.gate.add_permits(1);
        assert!(matches!(resolution.await, Err(Error::Stale)));
    });
}
fn native_session<'a>(login: &'a NativeLogin, _: &'a str) -> SessionFuture<'a, PyreSession> {
    Box::pin(async move {
        PyreSession::new(
            serde_json::json!({"userId": login.user_id}),
            &login.manifest.session_schema,
        )
        .ok()
    })
}

#[test]
fn public_consumer_runs_native_query_with_app_owned_connection_and_validators() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let manifest: Manifest = serde_json::from_value(serde_json::json!({
            "version": 1,
            "session_schema": {"userId": {"type": "Int", "nullable": false, "omittable": false}},
            "queries": {"visible": {
                "id": "visible", "operation": "query",
                "input_schema": {"id": {"type": "Int", "nullable": false, "omittable": false}},
                "session_args": ["userId"], "optional_input_args": [], "json_input_args": [],
                "sql": [{"include": true, "params": ["session_userId", "id"],
                    "sql": "SELECT json_group_array(json_object('body', body)) AS notes FROM notes WHERE ownerId = $session_userId AND id = $id"}]
            }}
        })).unwrap();
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let connection = Arc::new(tokio::sync::Mutex::new(db.connect().unwrap()));
        connection.lock().await.execute_batch("CREATE TABLE notes(id INTEGER, ownerId INTEGER, body TEXT); INSERT INTO notes VALUES (1, 1, 'private'), (2, 2, 'other');").await.unwrap();
        let login = NativeLogin { manifest: Arc::new(manifest), user_id: 1 };
        let manager = ContextManager::new(Config {
            get_session_key: |_: &NativeLogin| "trusted-login-id".to_owned(),
            resolve_session: native_session,
            get_database: |_: String| std::future::ready(Ok::<_, query::Error>(connection.clone())),
            max_age: Duration::from_secs(60),
        }).unwrap();
        let context = manager.get(&login, "notes").await.unwrap();
        let operation = |handle: Arc<tokio::sync::Mutex<libsql::Connection>>, session: Arc<PyreSession>, input| {
            let manifest = login.manifest.clone();
            async move {
                let connection = handle.lock().await;
                query::run(&connection, &manifest, "visible", input, &session).await
            }
        };
        let result: query::QueryResult = context.run(operation, serde_json::json!({"id": 1})).await.unwrap();
        assert!(result.response.to_string().contains("private"));
        assert!(!result.response.to_string().contains("other"));
        let hidden = context.run(operation, serde_json::json!({"id": 2})).await.unwrap();
        assert_eq!(hidden.response, serde_json::json!({"notes": []}));
        assert!(matches!(context.run(operation, serde_json::json!({"id": "invalid"})).await,
            Err(RunError::Operation(query::Error::InvalidInput(_)))));
    });
}
