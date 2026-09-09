//! Server-only cached database sessions. No connection, schema, or wire ownership.
//!
//! Applications must authenticate every request and supply an immutable global
//! session snapshot. `get_session_key` must return the trusted login/session ID,
//! not a user ID shared by multiple logins. Invalidate when authority changes;
//! cache hits deliberately do not re-resolve authority. Callbacks must not mutate
//! global sessions. Database handles and their locking remain application-owned.
//!
//! Concurrent misses may resolve more than once, but share the installed context.
//! Invalidation fences both pending resolutions and retained contexts. Validity
//! checks cannot revoke returned data or atomically coordinate external delivery.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// Borrowing asynchronous authority callback result.
pub type SessionFuture<'a, S> = Pin<Box<dyn Future<Output = Option<S>> + Send + 'a>>;

pub struct Config<K, R, D> {
    pub get_session_key: K,
    pub resolve_session: R,
    pub get_database: D,
    pub max_age: Duration,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidMaxAge,
    Denied,
    Capacity,
    Stale,
    Shutdown,
}

#[derive(Debug)]
pub enum RunError<E> {
    /// Operation was not invoked.
    Stale,
    /// Database lookup failed; operation was not invoked.
    Database(E),
    /// Original operation error, without changing its type.
    Operation(E),
    /// Authority expired or was invalidated during execution. The operation may
    /// have committed, even if it returned an error. Never automatically retry.
    StaleExecution { operation_error: Option<E> },
}

pub struct Context<S, D> {
    session: Arc<S>,
    database_id: String,
    get_database: Arc<D>,
    generation: Arc<AtomicBool>,
    expires_at: Instant,
}

impl<S, D> Context<S, D> {
    pub fn is_valid(&self) -> bool {
        self.generation.load(Ordering::Acquire) && Instant::now() < self.expires_at
    }

    /// Acquires an app-owned handle, checks validity again, then calls the
    /// operation with that handle, the pair-specific session, and input. Any
    /// waits inside the operation (including connection locks) are execution:
    /// invalidation there yields `StaleExecution`, not a safe-to-retry result.
    /// Lookup and operation use the same error type. After lookup, invalidation
    /// takes precedence over lookup failure; otherwise failure yields `Database`.
    pub async fn run<I, H, F, O, T, E>(
        &self,
        operation: impl FnOnce(H, Arc<S>, I) -> O,
        input: I,
    ) -> Result<T, RunError<E>>
    where
        D: Fn(String) -> F,
        F: Future<Output = Result<H, E>>,
        O: Future<Output = Result<T, E>>,
    {
        if !self.is_valid() {
            return Err(RunError::Stale);
        }
        let database = (self.get_database)(self.database_id.clone()).await;
        if !self.is_valid() {
            return Err(RunError::Stale);
        }
        let database = database.map_err(RunError::Database)?;
        let result = operation(database, self.session.clone(), input).await;
        if !self.is_valid() {
            return Err(RunError::StaleExecution {
                operation_error: result.err(),
            });
        }
        result.map_err(RunError::Operation)
    }
}

type Pair = (String, String);
struct Entry<S, D> {
    generation: Arc<AtomicBool>,
    context: Option<Arc<Context<S, D>>>,
    pending: usize,
}
struct State<S, D> {
    entries: HashMap<Pair, Entry<S, D>>,
    pending: usize,
    shutdown: bool,
}

pub struct ContextManager<S, K, R, D> {
    config: Config<K, R, Arc<D>>,
    state: Mutex<State<S, D>>,
}

// Drop also handles canceled/panicking resolution futures without leaking slots.
struct Pending<'a, S, D> {
    state: &'a Mutex<State<S, D>>,
    pair: Pair,
    generation: Arc<AtomicBool>,
}
impl<S, D> Drop for Pending<'_, S, D> {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.pending -= 1;
        if let Some(entry) = state.entries.get_mut(&self.pair) {
            if Arc::ptr_eq(&entry.generation, &self.generation) {
                entry.pending -= 1;
                if entry.pending == 0 && entry.context.is_none() {
                    state.entries.remove(&self.pair);
                }
            }
        }
    }
}

impl<S, K, R, D> ContextManager<S, K, R, D> {
    /// Cache and in-flight resolutions are internally capped at 1024 and 128.
    /// At capacity expired entries are reclaimed; live entries are not evicted.
    pub fn new(config: Config<K, R, D>) -> Result<Self, Error> {
        if config.max_age.is_zero() || Instant::now().checked_add(config.max_age).is_none() {
            return Err(Error::InvalidMaxAge);
        }
        Ok(Self {
            config: Config {
                get_session_key: config.get_session_key,
                resolve_session: config.resolve_session,
                get_database: Arc::new(config.get_database),
                max_age: config.max_age,
            },
            state: Mutex::new(State {
                entries: HashMap::new(),
                pending: 0,
                shutdown: false,
            }),
        })
    }

    pub async fn get<G>(
        &self,
        global_session: &G,
        database_id: &str,
    ) -> Result<Arc<Context<S, D>>, Error>
    where
        K: Fn(&G) -> String,
        R: for<'a> Fn(&'a G, &'a str) -> SessionFuture<'a, S>,
    {
        let pair = (
            (self.config.get_session_key)(global_session),
            database_id.to_owned(),
        );
        let pending = {
            let mut state = self.state.lock().unwrap();
            if state.shutdown {
                return Err(Error::Shutdown);
            }
            if let Some(context) = state.entries.get(&pair).and_then(|e| e.context.as_ref()) {
                if context.is_valid() {
                    return Ok(context.clone());
                }
                context.generation.store(false, Ordering::Release);
                state.entries.remove(&pair);
            }
            if state.pending >= 128 {
                return Err(Error::Capacity);
            }
            if state.entries.len() >= 1024 && !state.entries.contains_key(&pair) {
                state.entries.retain(|_, entry| {
                    let keep = entry.context.as_ref().is_none_or(|c| c.is_valid());
                    if !keep {
                        entry.generation.store(false, Ordering::Release);
                    }
                    keep
                });
                if state.entries.len() >= 1024 {
                    return Err(Error::Capacity);
                }
            }
            let entry = state.entries.entry(pair.clone()).or_insert_with(|| Entry {
                generation: Arc::new(AtomicBool::new(true)),
                context: None,
                pending: 0,
            });
            entry.pending += 1;
            let generation = entry.generation.clone();
            state.pending += 1;
            Pending {
                state: &self.state,
                pair,
                generation,
            }
        };
        // Time spent resolving consumes the authority lifetime, too.
        let expires_at = Instant::now()
            .checked_add(self.config.max_age)
            .ok_or(Error::InvalidMaxAge)?;
        let session = (self.config.resolve_session)(global_session, database_id).await;
        let mut state = self.state.lock().unwrap();
        if !pending.generation.load(Ordering::Acquire) || Instant::now() >= expires_at {
            return Err(Error::Stale);
        }
        let session = session.ok_or(Error::Denied)?;
        let entry = state.entries.get_mut(&pending.pair).unwrap();
        if let Some(context) = &entry.context {
            if !context.is_valid() {
                return Err(Error::Stale);
            }
            return Ok(context.clone());
        }
        let context = Arc::new(Context {
            session: Arc::new(session),
            database_id: database_id.into(),
            get_database: self.config.get_database.clone(),
            generation: pending.generation.clone(),
            expires_at,
        });
        entry.context = Some(context.clone());
        Ok(context)
    }

    fn invalidate(&self, matches: impl Fn(&Pair) -> bool, shutdown: bool) {
        let mut state = self.state.lock().unwrap();
        state.shutdown |= shutdown;
        state.entries.retain(|pair, entry| {
            if matches(pair) {
                entry.generation.store(false, Ordering::Release);
                false
            } else {
                true
            }
        });
    }

    pub fn invalidate_session(&self, session_key: &str) {
        self.invalidate(|(session, _)| session == session_key, false);
    }

    pub fn invalidate_database(&self, database_id: &str) {
        self.invalidate(|(_, database)| database == database_id, false);
    }

    pub fn invalidate_pair(&self, session_key: &str, database_id: &str) {
        self.invalidate(
            |(session, database)| session == session_key && database == database_id,
            false,
        );
    }

    pub fn shutdown(&self) {
        self.invalidate(|_| true, true);
    }
}

impl<S, K, R, D> Drop for ContextManager<S, K, R, D> {
    fn drop(&mut self) {
        self.shutdown();
    }
}
