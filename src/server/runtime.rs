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
    pub environment: Arc<dyn RuntimeEnvironment>,
}

impl fmt::Debug for RuntimeConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeConfig")
            .field("shared_write_policy", &self.shared_write_policy)
            .field("max_participants", &self.max_participants)
            .field("lease_duration", &self.lease_duration)
            .finish_non_exhaustive()
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            shared_write_policy: SharedWritePolicy::ServerOnly,
            max_participants: 1_024,
            lease_duration: Duration::from_secs(30),
            environment: Arc::new(SystemEnvironment::default()),
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

#[derive(Clone, Debug, PartialEq)]
pub struct JoinResult {
    pub participant: Participant,
    pub snapshot: Snapshot,
    /// The complete entry to publish to participants registered before this join.
    pub change: Change,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease {
    pub deadline_millis: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeError {
    EmptyDatabaseId,
    EmptyOwnerId,
    InvalidParticipantBound,
    InvalidLeaseDuration,
    ClockOverflow,
    IdGeneration(String),
    IdCollision,
    Capacity,
    Closed,
    WrongDatabase,
    StaleEpoch,
    UnknownConnection,
    StaleParticipant,
    OwnerMismatch,
    ReadOnly,
    SharedServerOnly,
    LeaseExpired,
    RevisionExhausted,
    GenerationExhausted,
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
            Self::ClockOverflow => formatter.write_str("lease deadline exceeds the clock range"),
            Self::IdGeneration(message) => write!(formatter, "failed to generate an ID: {message}"),
            Self::IdCollision => formatter.write_str("generated connection ID is already in use"),
            Self::Capacity => formatter.write_str("database runtime is at participant capacity"),
            Self::Closed => formatter.write_str("database runtime is closed"),
            Self::WrongDatabase => formatter.write_str("participant belongs to another database"),
            Self::StaleEpoch => formatter.write_str("participant belongs to a stale runtime epoch"),
            Self::UnknownConnection => formatter.write_str("connection is not active"),
            Self::StaleParticipant => formatter.write_str("participant generation is stale"),
            Self::OwnerMismatch => formatter.write_str("trusted owner does not own the connection"),
            Self::ReadOnly => formatter.write_str("participant is read-only"),
            Self::SharedServerOnly => formatter.write_str("Shared state is server-writable only"),
            Self::LeaseExpired => formatter.write_str("participant lease has expired"),
            Self::RevisionExhausted => formatter.write_str("runtime revision is exhausted"),
            Self::GenerationExhausted => formatter.write_str("participant generation is exhausted"),
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

struct State {
    revision: u64,
    generation: u64,
    shared: Option<Value>,
    connections: BTreeMap<String, Connection>,
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
        let lease_millis = duration_millis(config.lease_duration)?;
        if lease_millis == 0 {
            return Err(RuntimeError::InvalidLeaseDuration);
        }
        let now = config.environment.now();
        now.monotonic_millis
            .checked_add(lease_millis)
            .ok_or(RuntimeError::ClockOverflow)?;
        let shared = if contract.shared.is_some() {
            Some(contract.initialize_shared(now.unix_seconds)?)
        } else {
            None
        };
        let epoch = environment_id(config.environment.as_ref())?;
        Ok(Self {
            database_id,
            database,
            contract,
            epoch,
            config,
            state: Mutex::new(State {
                revision: 0,
                generation: 0,
                shared,
                connections: BTreeMap::new(),
                closed: false,
            }),
        })
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

    pub fn join(&self, evidence: JoinEvidence) -> Result<JoinResult, RuntimeError> {
        let mut state = self.lock_state()?;
        ensure_open(&state)?;
        self.require_state("Connection")?;
        if state.connections.len() >= self.config.max_participants {
            return Err(RuntimeError::Capacity);
        }
        require_owner_id(&evidence.owner_id)?;
        let now = self.config.environment.now();
        let value = self
            .contract
            .initialize_connection(&evidence.trusted_session, now.unix_seconds)?;
        let connection_id = self.new_id()?;
        let deadline = self.deadline(now)?;
        if state.connections.contains_key(&connection_id) {
            return Err(RuntimeError::IdCollision);
        }
        let generation = state
            .generation
            .checked_add(1)
            .ok_or(RuntimeError::GenerationExhausted)?;
        let revision = advance(&mut state)?;
        state.generation = generation;
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
        let change = self.connection_change(revision, connection_id, value);
        let snapshot = self.snapshot_locked(&state)?;
        Ok(JoinResult {
            participant,
            snapshot,
            change,
        })
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
        let revision = advance(&mut state)?;
        state
            .connections
            .get_mut(&participant.connection_id)
            .expect("validated connection")
            .value = patched.value.clone();
        Ok(Some(self.connection_change(
            revision,
            participant.connection_id.clone(),
            patched.value,
        )))
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
        let revision = advance(&mut state)?;
        state
            .connections
            .get_mut(&participant.connection_id)
            .expect("validated connection")
            .value = refreshed.value.clone();
        Ok(Some(self.connection_change(
            revision,
            participant.connection_id.clone(),
            refreshed.value,
        )))
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

    pub fn leave(&self, participant: &Participant) -> Result<Option<Change>, RuntimeError> {
        let mut state = self.lock_state()?;
        ensure_open(&state)?;
        self.validate_handle(&state, participant)?;
        let revision = advance(&mut state)?;
        state.connections.remove(&participant.connection_id);
        Ok(Some(self.removal_change(
            revision,
            vec![participant.connection_id.clone()],
        )))
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
        let now = self.config.environment.now();
        let mut state = self.lock_state()?;
        ensure_open(&state)?;
        let removed: Vec<_> = state
            .connections
            .iter()
            .filter(|(_, connection)| now.monotonic_millis >= connection.lease_deadline)
            .map(|(id, _)| id.clone())
            .collect();
        if removed.is_empty() {
            return Ok(None);
        }
        let revision = advance(&mut state)?;
        for id in &removed {
            state.connections.remove(id);
        }
        Ok(Some(self.removal_change(revision, removed)))
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
        let revision = advance(state)?;
        state.shared = Some(patched.value.clone());
        Ok(Some(Change {
            database_id: self.database_id.clone(),
            epoch: self.epoch.clone(),
            revision,
            shared: Some(patched.value),
            connections: BTreeMap::new(),
            removed_connections: Vec::new(),
        }))
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

    fn snapshot_locked(&self, state: &State) -> Result<Snapshot, RuntimeError> {
        ensure_open(state)?;
        Ok(Snapshot {
            database_id: self.database_id.clone(),
            epoch: self.epoch.clone(),
            revision: state.revision,
            shared: state.shared.clone(),
            connections: state
                .connections
                .iter()
                .map(|(id, connection)| (id.clone(), connection.value.clone()))
                .collect(),
        })
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

fn advance(state: &mut State) -> Result<u64, RuntimeError> {
    state.revision = state
        .revision
        .checked_add(1)
        .ok_or(RuntimeError::RevisionExhausted)?;
    Ok(state.revision)
}

fn duration_millis(duration: Duration) -> Result<u64, RuntimeError> {
    duration
        .as_millis()
        .try_into()
        .map_err(|_| RuntimeError::InvalidLeaseDuration)
}
