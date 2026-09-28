//! Application-owned lifecycle for one concrete database and its ephemeral state.
//!
//! `DatabaseRuntime` deliberately has no global registry. Applications retain one
//! instance alongside each resident database handle and use their authorization
//! boundary to construct trusted join/refresh evidence.

use crate::ephemeral::{Contract, ValidationError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedWritePolicy {
    ServerOnly,
    ParticipantWritable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeTime {
    /// Monotonic milliseconds used only for leases.
    pub monotonic_millis: u64,
    /// Unix seconds used by schema `now` defaults.
    pub unix_seconds: i64,
}

pub trait RuntimeEnvironment: Send + Sync {
    fn now(&self) -> RuntimeTime;
    fn new_id(&self) -> Result<String, String>;
}

#[derive(Debug)]
pub struct SystemEnvironment {
    started: Instant,
}

impl Default for SystemEnvironment {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl RuntimeEnvironment for SystemEnvironment {
    fn now(&self) -> RuntimeTime {
        RuntimeTime {
            monotonic_millis: self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .min(i64::MAX as u64) as i64,
        }
    }

    fn new_id(&self) -> Result<String, String> {
        let mut bytes = [0_u8; 16];
        getrandom::getrandom(&mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

#[derive(Clone)]
pub struct RuntimeConfig {
    pub shared_write_policy: SharedWritePolicy,
    pub max_participants: usize,
    pub lease_duration: Duration,
    pub downstream_delivery_cadence: Duration,
    pub max_pending_entries: usize,
    pub max_pending_controls: usize,
    pub max_delivery_bytes: usize,
    pub max_pending_bytes: usize,
    pub environment: Arc<dyn RuntimeEnvironment>,
}

impl fmt::Debug for RuntimeConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeConfig")
            .field("shared_write_policy", &self.shared_write_policy)
            .field("max_participants", &self.max_participants)
            .field("lease_duration", &self.lease_duration)
            .field(
                "downstream_delivery_cadence",
                &self.downstream_delivery_cadence,
            )
            .field("max_pending_entries", &self.max_pending_entries)
            .field("max_pending_controls", &self.max_pending_controls)
            .field("max_delivery_bytes", &self.max_delivery_bytes)
            .field("max_pending_bytes", &self.max_pending_bytes)
            .finish_non_exhaustive()
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self::with_environment(Arc::new(SystemEnvironment::default()))
    }
}

impl RuntimeConfig {
    pub fn with_environment(environment: Arc<dyn RuntimeEnvironment>) -> Self {
        Self {
            shared_write_policy: SharedWritePolicy::ServerOnly,
            max_participants: 256,
            lease_duration: Duration::from_secs(30),
            downstream_delivery_cadence: Duration::from_millis(50),
            max_pending_entries: 256,
            max_pending_controls: 1,
            max_delivery_bytes: 256 * 1024,
            max_pending_bytes: 8 * 1024 * 1024,
            environment,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct JoinEvidence {
    pub owner_id: String,
    pub trusted_session: Value,
    pub writable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Participant {
    database_id: String,
    epoch: String,
    connection_id: String,
    generation: u64,
    owner_id: String,
}

impl Participant {
    pub fn database_id(&self) -> &str {
        &self.database_id
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subscription {
    database_id: String,
    epoch: String,
    connection_id: String,
    generation: u64,
    writable: bool,
}

impl Subscription {
    pub fn database_id(&self) -> &str {
        &self.database_id
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub database_id: String,
    pub epoch: String,
    pub revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared: Option<Value>,
    pub connections: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub database_id: String,
    pub epoch: String,
    pub revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared: Option<Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connections: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_connections: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recovery {
    pub database_id: String,
    pub epoch: String,
    pub revision: u64,
}

/// Serializable transport messages. These revisions are scoped only to the
/// resident ephemeral runtime and are unrelated to durable sync cursors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Delivery {
    #[serde(rename = "ephemeralSnapshot")]
    Snapshot {
        #[serde(rename = "ephemeralSnapshot")]
        snapshot: Snapshot,
    },
    #[serde(rename = "ephemeralChanges")]
    Changes {
        #[serde(rename = "ephemeralChanges")]
        change: Change,
    },
    #[serde(rename = "ephemeralResyncRequired")]
    ResyncRequired {
        #[serde(rename = "ephemeralResyncRequired")]
        recovery: Recovery,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct JoinResult {
    pub participant: Participant,
    pub subscription: Subscription,
    pub snapshot: Snapshot,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubscriptionResult {
    pub subscription: Subscription,
    pub snapshot: Snapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease {
    pub deadline_millis: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expiration {
    pub change: Option<Change>,
    pub connection_ids: Vec<String>,
    pub subscription_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeError {
    EmptyDatabaseId,
    EmptyOwnerId,
    InvalidParticipantBound,
    InvalidLeaseDuration,
    InvalidTransportBounds,
    ClockOverflow,
    IdGeneration(String),
    IdCollision,
    Capacity,
    Closed,
    WrongDatabase,
    StaleEpoch,
    UnknownConnection,
    StaleParticipant,
    UnknownSubscription,
    StaleSubscription,
    OwnerMismatch,
    ReadOnly,
    SharedServerOnly,
    LeaseExpired,
    RevisionExhausted,
    GenerationExhausted,
    PayloadTooLarge,
    StateNotDeclared(&'static str),
    StatePoisoned,
    Validation(Vec<ValidationError>),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDatabaseId => formatter.write_str("database ID cannot be empty"),
            Self::EmptyOwnerId => formatter.write_str("trusted owner ID cannot be empty"),
            Self::InvalidParticipantBound => {
                formatter.write_str("maximum participant count must be greater than zero")
            }
            Self::InvalidLeaseDuration => formatter.write_str("lease duration is invalid"),
            Self::InvalidTransportBounds => {
                formatter.write_str("transport bounds must be greater than zero")
            }
            Self::ClockOverflow => formatter.write_str("runtime deadline exceeds the clock range"),
            Self::IdGeneration(message) => write!(formatter, "failed to generate an ID: {message}"),
            Self::IdCollision => formatter.write_str("generated connection ID is already in use"),
            Self::Capacity => formatter.write_str("database runtime is at participant capacity"),
            Self::Closed => formatter.write_str("database runtime is closed"),
            Self::WrongDatabase => formatter.write_str("participant belongs to another database"),
            Self::StaleEpoch => formatter.write_str("participant belongs to a stale runtime epoch"),
            Self::UnknownConnection => formatter.write_str("connection is not active"),
            Self::StaleParticipant => formatter.write_str("participant generation is stale"),
            Self::UnknownSubscription => formatter.write_str("subscription is not active"),
            Self::StaleSubscription => formatter.write_str("subscription generation is stale"),
            Self::OwnerMismatch => formatter.write_str("trusted owner does not own the connection"),
            Self::ReadOnly => formatter.write_str("participant is read-only"),
            Self::SharedServerOnly => formatter.write_str("Shared state is server-writable only"),
            Self::LeaseExpired => formatter.write_str("participant lease has expired"),
            Self::RevisionExhausted => formatter.write_str("runtime revision is exhausted"),
            Self::GenerationExhausted => formatter.write_str("participant generation is exhausted"),
            Self::PayloadTooLarge => {
                formatter.write_str("ephemeral delivery exceeds the payload bound")
            }
            Self::StateNotDeclared(name) => {
                write!(formatter, "ephemeral state '{name}' is not declared")
            }
            Self::StatePoisoned => formatter.write_str("database runtime state mutex is poisoned"),
            Self::Validation(errors) => {
                write!(formatter, "ephemeral state validation failed")?;
                if let Some(error) = errors.first() {
                    write!(formatter, ": {}", error.message)?;
                }
                Ok(())
            }
        }
    }
}

impl Error for RuntimeError {}

impl RuntimeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::EmptyDatabaseId => "empty_database_id",
            Self::EmptyOwnerId => "empty_owner_id",
            Self::InvalidParticipantBound => "invalid_participant_bound",
            Self::InvalidLeaseDuration => "invalid_lease_duration",
            Self::InvalidTransportBounds => "invalid_transport_bounds",
            Self::ClockOverflow => "clock_overflow",
            Self::IdGeneration(_) => "id_generation",
            Self::IdCollision => "id_collision",
            Self::Capacity => "capacity",
            Self::Closed => "closed",
            Self::WrongDatabase => "wrong_database",
            Self::StaleEpoch => "stale_epoch",
            Self::UnknownConnection => "unknown_connection",
            Self::StaleParticipant => "stale_participant",
            Self::UnknownSubscription => "unknown_subscription",
            Self::StaleSubscription => "stale_subscription",
            Self::OwnerMismatch => "owner_mismatch",
            Self::ReadOnly => "read_only",
            Self::SharedServerOnly => "shared_server_only",
            Self::LeaseExpired => "lease_expired",
            Self::RevisionExhausted => "revision_exhausted",
            Self::GenerationExhausted => "generation_exhausted",
            Self::PayloadTooLarge => "payload_too_large",
            Self::StateNotDeclared(_) => "state_not_declared",
            Self::StatePoisoned => "state_poisoned",
            Self::Validation(_) => "validation",
        }
    }
}

impl From<Vec<ValidationError>> for RuntimeError {
    fn from(errors: Vec<ValidationError>) -> Self {
        Self::Validation(errors)
    }
}

struct Connection {
    value: Value,
    owner_id: String,
    writable: bool,
    generation: u64,
    lease_deadline: u64,
}

#[derive(Clone)]
enum PendingConnection {
    Value(Value),
    Removed,
}

enum OutboxMode {
    Active {
        shared: Option<Value>,
        connections: BTreeMap<String, PendingConnection>,
        through_revision: Option<u64>,
    },
    ResyncRequired {
        revision: u64,
        delivered: bool,
    },
}

struct Outbox {
    generation: u64,
    owner_id: String,
    writable: bool,
    standalone_lease_deadline: Option<u64>,
    next_delivery_millis: u64,
    pending_bytes: usize,
    mode: OutboxMode,
}

struct State {
    revision: u64,
    generation: u64,
    subscription_generation: u64,
    shared: Option<Value>,
    connections: BTreeMap<String, Connection>,
    outboxes: BTreeMap<String, Outbox>,
    pending_bytes: usize,
    closed: bool,
}

pub struct DatabaseRuntime<H> {
    database_id: String,
    database: H,
    contract: Contract,
    epoch: String,
    config: RuntimeConfig,
    state: Mutex<State>,
}

impl<H> DatabaseRuntime<H> {
    pub fn new(
        database_id: impl Into<String>,
        database: H,
        contract: Contract,
        config: RuntimeConfig,
    ) -> Result<Self, RuntimeError> {
        let database_id = database_id.into();
        if database_id.trim().is_empty() {
            return Err(RuntimeError::EmptyDatabaseId);
        }
        if config.max_participants == 0 {
            return Err(RuntimeError::InvalidParticipantBound);
        }
        if config.max_pending_entries == 0
            || config.max_pending_controls == 0
            || config.max_delivery_bytes == 0
            || config.max_pending_bytes == 0
        {
            return Err(RuntimeError::InvalidTransportBounds);
        }
        let lease_millis = duration_millis(config.lease_duration)?;
        let cadence_millis = duration_millis(config.downstream_delivery_cadence)?;
        if lease_millis == 0 {
            return Err(RuntimeError::InvalidLeaseDuration);
        }
        let now = config.environment.now();
        now.monotonic_millis
            .checked_add(lease_millis)
            .ok_or(RuntimeError::ClockOverflow)?;
        now.monotonic_millis
            .checked_add(cadence_millis)
            .ok_or(RuntimeError::ClockOverflow)?;
        let shared = if contract.shared.is_some() {
            Some(contract.initialize_shared(now.unix_seconds)?)
        } else {
            None
        };
        let epoch = environment_id(config.environment.as_ref())?;
        let runtime = Self {
            database_id,
            database,
            contract,
            epoch,
            config,
            state: Mutex::new(State {
                revision: 0,
                generation: 0,
                subscription_generation: 0,
                shared,
                connections: BTreeMap::new(),
                outboxes: BTreeMap::new(),
                pending_bytes: 0,
                closed: false,
            }),
        };
        runtime.snapshot()?;
        Ok(runtime)
    }

    pub fn database_id(&self) -> &str {
        &self.database_id
    }

    pub fn database(&self) -> &H {
        &self.database
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    pub fn snapshot(&self) -> Result<Snapshot, RuntimeError> {
        let state = self.lock_state()?;
        self.snapshot_locked(&state)
    }

    pub fn revision(&self) -> Result<u64, RuntimeError> {
        let state = self.lock_state()?;
        ensure_open(&state)?;
        Ok(state.revision)
    }

    /// Registers a subscriber without creating Connection state.
    pub fn subscribe(
        &self,
        trusted_owner_id: &str,
        writable: bool,
    ) -> Result<SubscriptionResult, RuntimeError> {
        let now = self.config.environment.now();
        let mut state = self.lock_state()?;
        ensure_open(&state)?;
        self.require_state("Shared")?;
        require_owner_id(trusted_owner_id)?;
        if participation_count(&state) >= self.config.max_participants {
            return Err(RuntimeError::Capacity);
        }
        let snapshot = self.snapshot_locked(&state)?;
        let subscription_id = self.new_id()?;
        let deadline = self.deadline(now)?;
        if state.outboxes.contains_key(&subscription_id) {
            return Err(RuntimeError::IdCollision);
        }
        let generation = state
            .subscription_generation
            .checked_add(1)
            .ok_or(RuntimeError::GenerationExhausted)?;
        state.subscription_generation = generation;
        let subscription = self.insert_outbox_locked(
            &mut state,
            subscription_id,
            generation,
            trusted_owner_id.to_string(),
            writable,
            Some(deadline),
            now.monotonic_millis,
        )?;
        Ok(SubscriptionResult {
            subscription,
            snapshot,
        })
    }

    pub fn join(&self, evidence: JoinEvidence) -> Result<JoinResult, RuntimeError> {
        let mut state = self.lock_state()?;
        ensure_open(&state)?;
        self.require_state("Connection")?;
        if participation_count(&state) >= self.config.max_participants {
            return Err(RuntimeError::Capacity);
        }
        require_owner_id(&evidence.owner_id)?;
        let now = self.config.environment.now();
        let value = self
            .contract
            .initialize_connection(&evidence.trusted_session, now.unix_seconds)?;
        let connection_id = self.new_id()?;
        let deadline = self.deadline(now)?;
        now.monotonic_millis
            .checked_add(duration_millis(self.config.downstream_delivery_cadence)?)
            .ok_or(RuntimeError::ClockOverflow)?;
        if state.connections.contains_key(&connection_id)
            || state.outboxes.contains_key(&connection_id)
        {
            return Err(RuntimeError::IdCollision);
        }
        let generation = state
            .generation
            .checked_add(1)
            .ok_or(RuntimeError::GenerationExhausted)?;
        let subscription_generation = state
            .subscription_generation
            .checked_add(1)
            .ok_or(RuntimeError::GenerationExhausted)?;
        let revision = state
            .revision
            .checked_add(1)
            .ok_or(RuntimeError::RevisionExhausted)?;
        let mut snapshot = self.snapshot_value(&state);
        snapshot.revision = revision;
        snapshot
            .connections
            .insert(connection_id.clone(), value.clone());
        self.ensure_delivery_size(&Delivery::Snapshot {
            snapshot: snapshot.clone(),
        })?;
        let change = self.connection_change(revision, connection_id.clone(), value.clone());
        self.ensure_delivery_size(&Delivery::Changes {
            change: change.clone(),
        })?;
        state.revision = revision;
        state.generation = generation;
        state.subscription_generation = subscription_generation;
        let participant = Participant {
            database_id: self.database_id.clone(),
            epoch: self.epoch.clone(),
            connection_id: connection_id.clone(),
            generation,
            owner_id: evidence.owner_id.clone(),
        };
        state.connections.insert(
            connection_id.clone(),
            Connection {
                value: value.clone(),
                owner_id: evidence.owner_id,
                writable: evidence.writable,
                generation,
                lease_deadline: deadline,
            },
        );
        self.publish_locked(&mut state, &change);
        let subscription = self.insert_outbox_locked(
            &mut state,
            participant.connection_id.clone(),
            subscription_generation,
            participant.owner_id.clone(),
            evidence.writable,
            None,
            now.monotonic_millis,
        )?;
        Ok(JoinResult {
            participant,
            subscription,
            snapshot,
        })
    }

    /// Replaces a subscriber generation and atomically establishes a fresh
    /// snapshot boundary. The old handle becomes stale.
    pub fn resubscribe(
        &self,
        subscription: &Subscription,
        trusted_owner_id: &str,
    ) -> Result<SubscriptionResult, RuntimeError> {
        let now = self.config.environment.now();
        let mut state = self.lock_state()?;
        ensure_open(&state)?;
        self.validate_subscription(&state, subscription)?;
        require_owner_id(trusted_owner_id)?;
        if state
            .outboxes
            .get(&subscription.connection_id)
            .expect("validated subscription")
            .owner_id
            != trusted_owner_id
        {
            return Err(RuntimeError::OwnerMismatch);
        }
        let outbox = state
            .outboxes
            .get(&subscription.connection_id)
            .expect("validated subscription");
        let standalone_lease_deadline = outbox.standalone_lease_deadline;
        let writable = outbox.writable;
        if standalone_lease_deadline.is_some_and(|deadline| now.monotonic_millis >= deadline) {
            return Err(RuntimeError::LeaseExpired);
        }
        let snapshot = self.snapshot_locked(&state)?;
        now.monotonic_millis
            .checked_add(duration_millis(self.config.downstream_delivery_cadence)?)
            .ok_or(RuntimeError::ClockOverflow)?;
        let generation = state
            .subscription_generation
            .checked_add(1)
            .ok_or(RuntimeError::GenerationExhausted)?;
        state.subscription_generation = generation;
        let subscription = self.insert_outbox_locked(
            &mut state,
            subscription.connection_id.clone(),
            generation,
            trusted_owner_id.to_string(),
            writable,
            standalone_lease_deadline,
            now.monotonic_millis,
        )?;
        Ok(SubscriptionResult {
            subscription,
            snapshot,
        })
    }

    /// Drains at most one owned delivery. The returned value can be serialized
    /// or written after this method releases the runtime lock.
    pub fn poll(&self, subscription: &Subscription) -> Result<Option<Delivery>, RuntimeError> {
        let now = self.config.environment.now().monotonic_millis;
        let mut state = self.lock_state()?;
        ensure_open(&state)?;
        self.validate_subscription(&state, subscription)?;
        let State {
            outboxes,
            pending_bytes,
            ..
        } = &mut *state;
        let outbox = outboxes
            .get_mut(&subscription.connection_id)
            .expect("validated subscription");
        match &mut outbox.mode {
            OutboxMode::ResyncRequired {
                revision,
                delivered,
            } => {
                if *delivered {
                    return Ok(None);
                }
                *delivered = true;
                release_pending_bytes(pending_bytes, &mut outbox.pending_bytes);
                Ok(Some(Delivery::ResyncRequired {
                    recovery: Recovery {
                        database_id: self.database_id.clone(),
                        epoch: self.epoch.clone(),
                        revision: *revision,
                    },
                }))
            }
            OutboxMode::Active {
                shared,
                connections,
                through_revision,
            } => {
                let Some(revision) = *through_revision else {
                    return Ok(None);
                };
                if now < outbox.next_delivery_millis {
                    return Ok(None);
                }
                let next_delivery_millis = now
                    .checked_add(duration_millis(self.config.downstream_delivery_cadence)?)
                    .ok_or(RuntimeError::ClockOverflow)?;
                let mut values = BTreeMap::new();
                let mut removals = Vec::new();
                for (id, pending) in std::mem::take(connections) {
                    match pending {
                        PendingConnection::Value(value) => {
                            values.insert(id, value);
                        }
                        PendingConnection::Removed => removals.push(id),
                    }
                }
                let change = Change {
                    database_id: self.database_id.clone(),
                    epoch: self.epoch.clone(),
                    revision,
                    shared: shared.take(),
                    connections: values,
                    removed_connections: removals,
                };
                *through_revision = None;
                outbox.next_delivery_millis = next_delivery_millis;
                release_pending_bytes(pending_bytes, &mut outbox.pending_bytes);
                Ok(Some(Delivery::Changes { change }))
            }
        }
    }

    pub fn patch_connection(
        &self,
        participant: &Participant,
        trusted_owner_id: &str,
        patch: &Value,
    ) -> Result<Option<Change>, RuntimeError> {
        let now = self.config.environment.now();
        let mut state = self.lock_state()?;
        self.validate_participant(&state, participant, trusted_owner_id, now)?;
        let connection = state
            .connections
            .get(&participant.connection_id)
            .expect("validated connection");
        if !connection.writable {
            return Err(RuntimeError::ReadOnly);
        }
        let patched = self
            .contract
            .apply_patch("Connection", &connection.value, patch)?;
        if !patched.changed {
            return Ok(None);
        }
        let revision = next_revision(&state)?;
        let change = self.connection_change(
            revision,
            participant.connection_id.clone(),
            patched.value.clone(),
        );
        self.ensure_state_change_size(&state, &change)?;
        advance(&mut state)?;
        state
            .connections
            .get_mut(&participant.connection_id)
            .expect("validated connection")
            .value = patched.value.clone();
        self.publish_locked(&mut state, &change);
        Ok(Some(change))
    }

    pub fn refresh_connection(
        &self,
        participant: &Participant,
        trusted_owner_id: &str,
        trusted_session: &Value,
    ) -> Result<Option<Change>, RuntimeError> {
        let now = self.config.environment.now();
        let mut state = self.lock_state()?;
        self.validate_participant(&state, participant, trusted_owner_id, now)?;
        let current = &state
            .connections
            .get(&participant.connection_id)
            .expect("validated connection")
            .value;
        let refreshed =
            self.contract
                .refresh_connection(current, trusted_session, now.unix_seconds)?;
        if !refreshed.changed {
            return Ok(None);
        }
        let revision = next_revision(&state)?;
        let change = self.connection_change(
            revision,
            participant.connection_id.clone(),
            refreshed.value.clone(),
        );
        self.ensure_state_change_size(&state, &change)?;
        advance(&mut state)?;
        state
            .connections
            .get_mut(&participant.connection_id)
            .expect("validated connection")
            .value = refreshed.value.clone();
        self.publish_locked(&mut state, &change);
        Ok(Some(change))
    }

    /// Revalidates derived fields and renews the lease at one ordering boundary.
    pub fn refresh_and_renew(
        &self,
        participant: &Participant,
        trusted_owner_id: &str,
        trusted_session: &Value,
    ) -> Result<(Option<Change>, Lease), RuntimeError> {
        let now = self.config.environment.now();
        let mut state = self.lock_state()?;
        self.validate_participant(&state, participant, trusted_owner_id, now)?;
        let current = &state
            .connections
            .get(&participant.connection_id)
            .expect("validated connection")
            .value;
        let refreshed =
            self.contract
                .refresh_connection(current, trusted_session, now.unix_seconds)?;
        let deadline = self.deadline(now)?;
        let change = if refreshed.changed {
            let revision = next_revision(&state)?;
            let change = self.connection_change(
                revision,
                participant.connection_id.clone(),
                refreshed.value.clone(),
            );
            self.ensure_state_change_size(&state, &change)?;
            advance(&mut state)?;
            let connection = state
                .connections
                .get_mut(&participant.connection_id)
                .expect("validated connection");
            connection.value = refreshed.value.clone();
            connection.lease_deadline = deadline;
            self.publish_locked(&mut state, &change);
            Some(change)
        } else {
            state
                .connections
                .get_mut(&participant.connection_id)
                .expect("validated connection")
                .lease_deadline = deadline;
            None
        };
        Ok((
            change,
            Lease {
                deadline_millis: deadline,
            },
        ))
    }

    pub fn patch_shared_from_participant(
        &self,
        participant: &Participant,
        trusted_owner_id: &str,
        patch: &Value,
    ) -> Result<Option<Change>, RuntimeError> {
        let now = self.config.environment.now();
        let mut state = self.lock_state()?;
        self.validate_participant(&state, participant, trusted_owner_id, now)?;
        self.require_state("Shared")?;
        if self.config.shared_write_policy != SharedWritePolicy::ParticipantWritable {
            return Err(RuntimeError::SharedServerOnly);
        }
        if !state
            .connections
            .get(&participant.connection_id)
            .expect("validated connection")
            .writable
        {
            return Err(RuntimeError::ReadOnly);
        }
        self.patch_shared_locked(&mut state, patch)
    }

    pub fn patch_shared_from_subscription(
        &self,
        subscription: &Subscription,
        trusted_owner_id: &str,
        patch: &Value,
    ) -> Result<Option<Change>, RuntimeError> {
        let now = self.config.environment.now();
        let mut state = self.lock_state()?;
        ensure_open(&state)?;
        self.validate_subscription(&state, subscription)?;
        self.require_state("Shared")?;
        require_owner_id(trusted_owner_id)?;
        if self.config.shared_write_policy != SharedWritePolicy::ParticipantWritable {
            return Err(RuntimeError::SharedServerOnly);
        }
        let outbox = state
            .outboxes
            .get(&subscription.connection_id)
            .expect("validated subscription");
        if outbox.owner_id != trusted_owner_id {
            return Err(RuntimeError::OwnerMismatch);
        }
        let Some(deadline) = outbox.standalone_lease_deadline else {
            return Err(RuntimeError::UnknownConnection);
        };
        if now.monotonic_millis >= deadline {
            return Err(RuntimeError::LeaseExpired);
        }
        if !outbox.writable || !subscription.writable {
            return Err(RuntimeError::ReadOnly);
        }
        self.patch_shared_locked(&mut state, patch)
    }

    /// Trusted server code can update Shared regardless of participant policy.
    pub fn patch_shared(&self, patch: &Value) -> Result<Option<Change>, RuntimeError> {
        let mut state = self.lock_state()?;
        ensure_open(&state)?;
        self.require_state("Shared")?;
        self.patch_shared_locked(&mut state, patch)
    }

    pub fn renew(
        &self,
        participant: &Participant,
        trusted_owner_id: &str,
    ) -> Result<Lease, RuntimeError> {
        let now = self.config.environment.now();
        let mut state = self.lock_state()?;
        self.validate_participant(&state, participant, trusted_owner_id, now)?;
        let deadline = self.deadline(now)?;
        state
            .connections
            .get_mut(&participant.connection_id)
            .expect("validated connection")
            .lease_deadline = deadline;
        Ok(Lease {
            deadline_millis: deadline,
        })
    }

    pub fn renew_subscription(
        &self,
        subscription: &Subscription,
        trusted_owner_id: &str,
    ) -> Result<Lease, RuntimeError> {
        let now = self.config.environment.now();
        let mut state = self.lock_state()?;
        ensure_open(&state)?;
        self.validate_subscription(&state, subscription)?;
        require_owner_id(trusted_owner_id)?;
        let outbox = state
            .outboxes
            .get_mut(&subscription.connection_id)
            .expect("validated subscription");
        if outbox.owner_id != trusted_owner_id {
            return Err(RuntimeError::OwnerMismatch);
        }
        let Some(current_deadline) = outbox.standalone_lease_deadline else {
            return Err(RuntimeError::UnknownConnection);
        };
        if now.monotonic_millis >= current_deadline {
            return Err(RuntimeError::LeaseExpired);
        }
        let deadline = now
            .monotonic_millis
            .checked_add(duration_millis(self.config.lease_duration)?)
            .ok_or(RuntimeError::ClockOverflow)?;
        outbox.standalone_lease_deadline = Some(deadline);
        Ok(Lease {
            deadline_millis: deadline,
        })
    }

    pub fn leave(&self, participant: &Participant) -> Result<Option<Change>, RuntimeError> {
        let mut state = self.lock_state()?;
        ensure_open(&state)?;
        self.validate_handle(&state, participant)?;
        let revision = advance(&mut state)?;
        state.connections.remove(&participant.connection_id);
        remove_outbox_locked(&mut state, &participant.connection_id);
        let change = self.removal_change(revision, vec![participant.connection_id.clone()]);
        self.publish_locked(&mut state, &change);
        Ok(Some(change))
    }

    /// Removes a subscription that has no associated Connection value.
    /// Joined subscriptions return `UnknownConnection` and must be retired with `leave`.
    pub fn unsubscribe(&self, subscription: &Subscription) -> Result<(), RuntimeError> {
        let mut state = self.lock_state()?;
        ensure_open(&state)?;
        self.validate_subscription(&state, subscription)?;
        if state.connections.contains_key(&subscription.connection_id) {
            return Err(RuntimeError::UnknownConnection);
        }
        remove_outbox_locked(&mut state, &subscription.connection_id);
        Ok(())
    }

    /// Hook for a host transport's close notification.
    pub fn transport_closed(
        &self,
        participant: &Participant,
    ) -> Result<Option<Change>, RuntimeError> {
        self.leave(participant)
    }

    /// Hook for a host authorization refresh that revokes database access.
    pub fn authorization_lost(
        &self,
        participant: &Participant,
    ) -> Result<Option<Change>, RuntimeError> {
        self.leave(participant)
    }

    pub fn expire_leases(&self) -> Result<Option<Change>, RuntimeError> {
        Ok(self.expire_leases_detailed()?.change)
    }

    /// Expires leases and reports the opaque IDs whose host-side handles can be discarded.
    pub fn expire_leases_detailed(&self) -> Result<Expiration, RuntimeError> {
        let now = self.config.environment.now();
        let mut state = self.lock_state()?;
        ensure_open(&state)?;
        let removed: Vec<_> = state
            .connections
            .iter()
            .filter(|(_, connection)| now.monotonic_millis >= connection.lease_deadline)
            .map(|(id, _)| id.clone())
            .collect();
        let expired_subscriptions: Vec<_> = state
            .outboxes
            .iter()
            .filter(|(_, outbox)| {
                outbox
                    .standalone_lease_deadline
                    .is_some_and(|deadline| now.monotonic_millis >= deadline)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired_subscriptions {
            remove_outbox_locked(&mut state, id);
        }
        if removed.is_empty() {
            return Ok(Expiration {
                change: None,
                connection_ids: removed,
                subscription_ids: expired_subscriptions,
            });
        }
        let revision = advance(&mut state)?;
        for id in &removed {
            state.connections.remove(id);
            remove_outbox_locked(&mut state, id);
        }
        let change = self.removal_change(revision, removed.clone());
        self.publish_locked(&mut state, &change);
        Ok(Expiration {
            change: Some(change),
            connection_ids: removed,
            subscription_ids: expired_subscriptions,
        })
    }

    /// Fences all future work and discards Shared and Connection state.
    pub fn close(&self) -> Result<Option<Change>, RuntimeError> {
        let mut state = self.lock_state()?;
        if state.closed {
            return Ok(None);
        }
        let removed: Vec<_> = state.connections.keys().cloned().collect();
        let revision = if removed.is_empty() {
            None
        } else {
            Some(advance(&mut state)?)
        };
        state.closed = true;
        state.shared = None;
        state.connections.clear();
        state.outboxes.clear();
        state.pending_bytes = 0;
        Ok(revision.map(|revision| self.removal_change(revision, removed)))
    }

    fn patch_shared_locked(
        &self,
        state: &mut State,
        patch: &Value,
    ) -> Result<Option<Change>, RuntimeError> {
        let current = state
            .shared
            .as_ref()
            .ok_or(RuntimeError::StateNotDeclared("Shared"))?;
        let patched = self.contract.apply_patch("Shared", current, patch)?;
        if !patched.changed {
            return Ok(None);
        }
        let revision = next_revision(state)?;
        let change = Change {
            database_id: self.database_id.clone(),
            epoch: self.epoch.clone(),
            revision,
            shared: Some(patched.value.clone()),
            connections: BTreeMap::new(),
            removed_connections: Vec::new(),
        };
        self.ensure_state_change_size(state, &change)?;
        advance(state)?;
        state.shared = Some(patched.value.clone());
        self.publish_locked(state, &change);
        Ok(Some(change))
    }

    fn validate_participant(
        &self,
        state: &State,
        participant: &Participant,
        trusted_owner_id: &str,
        now: RuntimeTime,
    ) -> Result<(), RuntimeError> {
        ensure_open(state)?;
        self.validate_handle(state, participant)?;
        require_owner_id(trusted_owner_id)?;
        let connection = state
            .connections
            .get(&participant.connection_id)
            .expect("validated connection");
        if connection.owner_id != trusted_owner_id || participant.owner_id != trusted_owner_id {
            return Err(RuntimeError::OwnerMismatch);
        }
        if now.monotonic_millis >= connection.lease_deadline {
            return Err(RuntimeError::LeaseExpired);
        }
        Ok(())
    }

    fn validate_handle(
        &self,
        state: &State,
        participant: &Participant,
    ) -> Result<(), RuntimeError> {
        if participant.database_id != self.database_id {
            return Err(RuntimeError::WrongDatabase);
        }
        if participant.epoch != self.epoch {
            return Err(RuntimeError::StaleEpoch);
        }
        let connection = state
            .connections
            .get(&participant.connection_id)
            .ok_or(RuntimeError::UnknownConnection)?;
        if connection.generation != participant.generation {
            return Err(RuntimeError::StaleParticipant);
        }
        Ok(())
    }

    fn validate_subscription(
        &self,
        state: &State,
        subscription: &Subscription,
    ) -> Result<(), RuntimeError> {
        if subscription.database_id != self.database_id {
            return Err(RuntimeError::WrongDatabase);
        }
        if subscription.epoch != self.epoch {
            return Err(RuntimeError::StaleEpoch);
        }
        let outbox = state
            .outboxes
            .get(&subscription.connection_id)
            .ok_or(RuntimeError::UnknownSubscription)?;
        if outbox.generation != subscription.generation {
            return Err(RuntimeError::StaleSubscription);
        }
        Ok(())
    }

    fn snapshot_locked(&self, state: &State) -> Result<Snapshot, RuntimeError> {
        ensure_open(state)?;
        let snapshot = self.snapshot_value(state);
        self.ensure_delivery_size(&Delivery::Snapshot {
            snapshot: snapshot.clone(),
        })?;
        Ok(snapshot)
    }

    fn snapshot_value(&self, state: &State) -> Snapshot {
        Snapshot {
            database_id: self.database_id.clone(),
            epoch: self.epoch.clone(),
            revision: state.revision,
            shared: state.shared.clone(),
            connections: state
                .connections
                .iter()
                .map(|(id, connection)| (id.clone(), connection.value.clone()))
                .collect(),
        }
    }

    fn insert_outbox_locked(
        &self,
        state: &mut State,
        connection_id: String,
        generation: u64,
        owner_id: String,
        writable: bool,
        standalone_lease_deadline: Option<u64>,
        now_millis: u64,
    ) -> Result<Subscription, RuntimeError> {
        let next_delivery_millis = now_millis
            .checked_add(duration_millis(self.config.downstream_delivery_cadence)?)
            .ok_or(RuntimeError::ClockOverflow)?;
        let replaced = state.outboxes.insert(
            connection_id.clone(),
            Outbox {
                generation,
                owner_id,
                writable,
                standalone_lease_deadline,
                next_delivery_millis,
                pending_bytes: 0,
                mode: OutboxMode::Active {
                    shared: None,
                    connections: BTreeMap::new(),
                    through_revision: None,
                },
            },
        );
        if let Some(replaced) = replaced {
            state.pending_bytes = state
                .pending_bytes
                .checked_sub(replaced.pending_bytes)
                .expect("outbox pending bytes are included in runtime total");
        }
        Ok(Subscription {
            database_id: self.database_id.clone(),
            epoch: self.epoch.clone(),
            connection_id,
            generation,
            writable,
        })
    }

    fn publish_locked(&self, state: &mut State, change: &Change) {
        let mut total_pending_bytes = state.pending_bytes;
        for outbox in state.outboxes.values_mut() {
            let OutboxMode::Active {
                shared,
                connections,
                through_revision,
            } = &mut outbox.mode
            else {
                continue;
            };
            total_pending_bytes = total_pending_bytes
                .checked_sub(outbox.pending_bytes)
                .expect("outbox pending bytes are included in runtime total");
            if let Some(value) = &change.shared {
                *shared = Some(value.clone());
            }
            for (id, value) in &change.connections {
                connections.insert(id.clone(), PendingConnection::Value(value.clone()));
            }
            for id in &change.removed_connections {
                connections.insert(id.clone(), PendingConnection::Removed);
            }
            *through_revision = Some(change.revision);

            let entry_count = usize::from(shared.is_some()) + connections.len();
            let too_many_entries = entry_count > self.config.max_pending_entries;
            let pending_bytes = if too_many_entries {
                None
            } else {
                let pending = pending_delivery(
                    &self.database_id,
                    &self.epoch,
                    change.revision,
                    shared,
                    connections,
                );
                serde_json::to_vec(&pending).ok().map(|bytes| bytes.len())
            };
            let over_outbox_limit =
                pending_bytes.map_or(true, |bytes| bytes > self.config.max_delivery_bytes);
            let over_runtime_limit = pending_bytes.map_or(true, |bytes| {
                total_pending_bytes
                    .checked_add(bytes)
                    .map_or(true, |total| total > self.config.max_pending_bytes)
            });
            if too_many_entries || over_outbox_limit || over_runtime_limit {
                // There is exactly one bounded control slot per subscriber.
                debug_assert!(self.config.max_pending_controls >= 1);
                outbox.mode = OutboxMode::ResyncRequired {
                    revision: change.revision,
                    delivered: false,
                };
                outbox.pending_bytes = 0;
            } else {
                outbox.pending_bytes = pending_bytes.expect("checked above");
                total_pending_bytes = total_pending_bytes
                    .checked_add(outbox.pending_bytes)
                    .expect("pending byte limit prevents overflow");
            }
        }
        state.pending_bytes = total_pending_bytes;
    }

    fn ensure_delivery_size(&self, delivery: &Delivery) -> Result<(), RuntimeError> {
        let size = serde_json::to_vec(delivery)
            .map_err(|_| RuntimeError::PayloadTooLarge)?
            .len();
        if size > self.config.max_delivery_bytes {
            Err(RuntimeError::PayloadTooLarge)
        } else {
            Ok(())
        }
    }

    fn ensure_state_change_size(&self, state: &State, change: &Change) -> Result<(), RuntimeError> {
        self.ensure_delivery_size(&Delivery::Changes {
            change: change.clone(),
        })?;
        let mut snapshot = self.snapshot_value(state);
        snapshot.revision = change.revision;
        if let Some(shared) = &change.shared {
            snapshot.shared = Some(shared.clone());
        }
        snapshot.connections.extend(change.connections.clone());
        for id in &change.removed_connections {
            snapshot.connections.remove(id);
        }
        self.ensure_delivery_size(&Delivery::Snapshot { snapshot })
    }

    fn connection_change(&self, revision: u64, id: String, value: Value) -> Change {
        Change {
            database_id: self.database_id.clone(),
            epoch: self.epoch.clone(),
            revision,
            shared: None,
            connections: [(id, value)].into_iter().collect(),
            removed_connections: Vec::new(),
        }
    }

    fn removal_change(&self, revision: u64, removed_connections: Vec<String>) -> Change {
        Change {
            database_id: self.database_id.clone(),
            epoch: self.epoch.clone(),
            revision,
            shared: None,
            connections: BTreeMap::new(),
            removed_connections,
        }
    }

    fn deadline(&self, now: RuntimeTime) -> Result<u64, RuntimeError> {
        now.monotonic_millis
            .checked_add(duration_millis(self.config.lease_duration)?)
            .ok_or(RuntimeError::ClockOverflow)
    }

    fn new_id(&self) -> Result<String, RuntimeError> {
        environment_id(self.config.environment.as_ref())
    }

    fn require_state(&self, name: &'static str) -> Result<(), RuntimeError> {
        let declared = match name {
            "Connection" => self.contract.connection.is_some(),
            "Shared" => self.contract.shared.is_some(),
            _ => false,
        };
        if declared {
            Ok(())
        } else {
            Err(RuntimeError::StateNotDeclared(name))
        }
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, State>, RuntimeError> {
        self.state.lock().map_err(|_| RuntimeError::StatePoisoned)
    }
}

fn environment_id(environment: &dyn RuntimeEnvironment) -> Result<String, RuntimeError> {
    let id = environment.new_id().map_err(RuntimeError::IdGeneration)?;
    if id.trim().is_empty() {
        Err(RuntimeError::IdGeneration(
            "environment returned an empty id".to_string(),
        ))
    } else {
        Ok(id)
    }
}

fn ensure_open(state: &State) -> Result<(), RuntimeError> {
    if state.closed {
        Err(RuntimeError::Closed)
    } else {
        Ok(())
    }
}

fn require_owner_id(owner_id: &str) -> Result<(), RuntimeError> {
    if owner_id.trim().is_empty() {
        Err(RuntimeError::EmptyOwnerId)
    } else {
        Ok(())
    }
}

fn participation_count(state: &State) -> usize {
    state.connections.len()
        + state
            .outboxes
            .keys()
            .filter(|id| !state.connections.contains_key(*id))
            .count()
}

fn release_pending_bytes(total: &mut usize, outbox: &mut usize) {
    *total = total
        .checked_sub(*outbox)
        .expect("outbox pending bytes are included in runtime total");
    *outbox = 0;
}

fn remove_outbox_locked(state: &mut State, id: &str) -> Option<Outbox> {
    let outbox = state.outboxes.remove(id)?;
    state.pending_bytes = state
        .pending_bytes
        .checked_sub(outbox.pending_bytes)
        .expect("outbox pending bytes are included in runtime total");
    Some(outbox)
}

fn advance(state: &mut State) -> Result<u64, RuntimeError> {
    state.revision = next_revision(state)?;
    Ok(state.revision)
}

fn next_revision(state: &State) -> Result<u64, RuntimeError> {
    state
        .revision
        .checked_add(1)
        .ok_or(RuntimeError::RevisionExhausted)
}

fn duration_millis(duration: Duration) -> Result<u64, RuntimeError> {
    duration
        .as_millis()
        .try_into()
        .map_err(|_| RuntimeError::InvalidLeaseDuration)
}

fn pending_delivery(
    database_id: &str,
    epoch: &str,
    revision: u64,
    shared: &Option<Value>,
    connections: &BTreeMap<String, PendingConnection>,
) -> Delivery {
    let mut values = BTreeMap::new();
    let mut removals = Vec::new();
    for (id, pending) in connections {
        match pending {
            PendingConnection::Value(value) => {
                values.insert(id.clone(), value.clone());
            }
            PendingConnection::Removed => removals.push(id.clone()),
        }
    }
    Delivery::Changes {
        change: Change {
            database_id: database_id.to_string(),
            epoch: epoch.to_string(),
            revision,
            shared: shared.clone(),
            connections: values,
            removed_connections: removals,
        },
    }
}
