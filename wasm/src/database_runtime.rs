use pyre::{
    ephemeral::{Contract, ValidationError},
    server::runtime::{
        Change, DatabaseRuntime, JoinEvidence, Lease, Participant, RuntimeConfig,
        RuntimeEnvironment, RuntimeError, RuntimeTime, SharedWritePolicy, Snapshot, Subscription,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use js_sys::{Function, Reflect};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use wasm_bindgen::prelude::*;

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct JsRuntimeConfig {
    shared_write_policy: Option<JsSharedWritePolicy>,
    max_participants: Option<usize>,
    lease_duration_ms: Option<u64>,
    downstream_delivery_cadence_ms: Option<u64>,
    max_pending_entries: Option<usize>,
    max_pending_controls: Option<usize>,
    max_delivery_bytes: Option<usize>,
    max_pending_bytes: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum JsSharedWritePolicy {
    ServerOnly,
    ParticipantWritable,
}

impl JsRuntimeConfig {
    fn into_runtime(self) -> RuntimeConfig {
        let mut config = RuntimeConfig::with_environment(Arc::new(WasmEnvironment::default()));
        if let Some(policy) = self.shared_write_policy {
            config.shared_write_policy = match policy {
                JsSharedWritePolicy::ServerOnly => SharedWritePolicy::ServerOnly,
                JsSharedWritePolicy::ParticipantWritable => SharedWritePolicy::ParticipantWritable,
            };
        }
        if let Some(value) = self.max_participants {
            config.max_participants = value;
        }
        if let Some(value) = self.lease_duration_ms {
            config.lease_duration = Duration::from_millis(value);
        }
        if let Some(value) = self.downstream_delivery_cadence_ms {
            config.downstream_delivery_cadence = Duration::from_millis(value);
        }
        if let Some(value) = self.max_pending_entries {
            config.max_pending_entries = value;
        }
        if let Some(value) = self.max_pending_controls {
            config.max_pending_controls = value;
        }
        if let Some(value) = self.max_delivery_bytes {
            config.max_delivery_bytes = value;
        }
        if let Some(value) = self.max_pending_bytes {
            config.max_pending_bytes = value;
        }
        config
    }
}

struct WasmEnvironment {
    started_millis: u64,
    uses_performance_clock: bool,
    last_millis: AtomicU64,
}

impl Default for WasmEnvironment {
    fn default() -> Self {
        let performance_millis = performance_clock_millis();
        Self {
            started_millis: performance_millis.unwrap_or_else(wall_clock_millis),
            uses_performance_clock: performance_millis.is_some(),
            last_millis: AtomicU64::new(0),
        }
    }
}

impl RuntimeEnvironment for WasmEnvironment {
    fn now(&self) -> RuntimeTime {
        let current = if self.uses_performance_clock {
            performance_clock_millis()
        } else {
            Some(wall_clock_millis())
        };
        let previous = self.last_millis.load(Ordering::Relaxed);
        let elapsed = monotonic_elapsed(self.started_millis, current, previous);
        let monotonic_millis = self
            .last_millis
            .fetch_max(elapsed, Ordering::Relaxed)
            .max(elapsed);
        RuntimeTime {
            monotonic_millis,
            unix_seconds: (js_sys::Date::now() / 1_000.0).floor() as i64,
        }
    }

    fn new_id(&self) -> Result<String, String> {
        let mut bytes = [0_u8; 16];
        getrandom::getrandom(&mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

fn wall_clock_millis() -> u64 {
    js_sys::Date::now().max(0.0).min(u64::MAX as f64) as u64
}

fn performance_clock_millis() -> Option<u64> {
    let global = js_sys::global();
    let performance = Reflect::get(&global, &JsValue::from_str("performance")).ok()?;
    if performance.is_null() || performance.is_undefined() {
        return None;
    }
    let now = Reflect::get(&performance, &JsValue::from_str("now")).ok()?;
    let now: Function = now.dyn_into().ok()?;
    let millis = now.call0(&performance).ok()?.as_f64()?;
    if !millis.is_finite() || millis < 0.0 {
        return None;
    }
    Some(millis.min(u64::MAX as f64) as u64)
}

fn monotonic_elapsed(started_millis: u64, current_millis: Option<u64>, previous: u64) -> u64 {
    current_millis
        .map(|current| current.saturating_sub(started_millis))
        .unwrap_or(previous)
        .max(previous)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BridgeError {
    code: String,
    message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    validation_errors: Vec<ValidationError>,
}

impl From<RuntimeError> for BridgeError {
    fn from(error: RuntimeError) -> Self {
        let validation_errors = match &error {
            RuntimeError::Validation(errors) => errors.clone(),
            _ => Vec::new(),
        };
        Self {
            code: error.code().to_string(),
            message: error.to_string(),
            validation_errors,
        }
    }
}

impl BridgeError {
    fn input(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            validation_errors: Vec::new(),
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum Response<T> {
    Ok { ok: bool, value: T },
    Err { ok: bool, error: BridgeError },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Connected {
    connection_id: String,
    handle: String,
    snapshot: Snapshot,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Refreshed {
    change: Option<Change>,
    lease: LeaseValue,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Expired {
    change: Option<Change>,
    connection_ids: Vec<String>,
    subscription_ids: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LeaseValue {
    deadline_millis: u64,
}

impl From<Lease> for LeaseValue {
    fn from(lease: Lease) -> Self {
        Self {
            deadline_millis: lease.deadline_millis,
        }
    }
}

pub(crate) struct RuntimeBridge {
    runtime: DatabaseRuntime<()>,
    participants: BTreeMap<String, Participant>,
    subscriptions: BTreeMap<String, Subscription>,
}

impl RuntimeBridge {
    fn new(
        database_id: String,
        contract: Contract,
        config: RuntimeConfig,
    ) -> Result<Self, BridgeError> {
        Ok(Self {
            runtime: DatabaseRuntime::new(database_id, (), contract, config)?,
            participants: BTreeMap::new(),
            subscriptions: BTreeMap::new(),
        })
    }

    fn join(
        &mut self,
        owner_id: String,
        trusted_session: Value,
        writable: bool,
    ) -> Result<Connected, BridgeError> {
        let handle = self.new_handle()?;
        let joined = self.runtime.join(JoinEvidence {
            owner_id,
            trusted_session,
            writable,
        })?;
        let connection_id = joined.participant.connection_id().to_string();
        self.participants.insert(handle.clone(), joined.participant);
        self.subscriptions.insert(handle.clone(), joined.subscription);
        Ok(Connected {
            connection_id,
            handle,
            snapshot: joined.snapshot,
        })
    }

    fn subscribe(&mut self, owner_id: &str, writable: bool) -> Result<Connected, BridgeError> {
        let handle = self.new_handle()?;
        let subscribed = self.runtime.subscribe(owner_id, writable)?;
        let connection_id = subscribed.subscription.connection_id().to_string();
        self.subscriptions.insert(handle.clone(), subscribed.subscription);
        Ok(Connected {
            connection_id,
            handle,
            snapshot: subscribed.snapshot,
        })
    }

    fn participant(&self, handle: &str) -> Result<&Participant, BridgeError> {
        self.participants
            .get(handle)
            .ok_or_else(|| BridgeError::from(RuntimeError::UnknownConnection))
    }

    fn subscription(&self, handle: &str) -> Result<&Subscription, BridgeError> {
        self.subscriptions
            .get(handle)
            .ok_or_else(|| BridgeError::from(RuntimeError::UnknownSubscription))
    }

    fn new_handle(&self) -> Result<String, BridgeError> {
        for _ in 0..4 {
            let mut bytes = [0_u8; 32];
            getrandom::getrandom(&mut bytes)
                .map_err(|error| BridgeError::from(RuntimeError::IdGeneration(error.to_string())))?;
            let handle: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            if !self.participants.contains_key(&handle) && !self.subscriptions.contains_key(&handle)
            {
                return Ok(handle);
            }
        }
        Err(BridgeError::from(RuntimeError::IdCollision))
    }

    fn expire(&mut self) -> Result<Expired, BridgeError> {
        let expired = self.runtime.expire_leases_detailed()?;
        self.participants.retain(|_, participant| {
            !expired
                .connection_ids
                .iter()
                .any(|id| id == participant.connection_id())
        });
        self.subscriptions.retain(|_, subscription| {
            !expired
                .connection_ids
                .iter()
                .chain(&expired.subscription_ids)
                .any(|id| id == subscription.connection_id())
        });
        Ok(Expired {
            change: expired.change,
            connection_ids: expired.connection_ids,
            subscription_ids: expired.subscription_ids,
        })
    }
}

#[wasm_bindgen]
pub struct WasmDatabaseRuntime {
    bridge: RuntimeBridge,
}

#[wasm_bindgen]
impl WasmDatabaseRuntime {
    #[wasm_bindgen(constructor)]
    pub fn new(database_id: String, contract: JsValue, config: JsValue) -> Result<Self, JsValue> {
        let contract = from_js::<Contract>(contract, "invalid_contract")?;
        let config = if config.is_null() || config.is_undefined() {
            JsRuntimeConfig::default()
        } else {
            from_js::<JsRuntimeConfig>(config, "invalid_config")?
        };
        RuntimeBridge::new(database_id, contract, config.into_runtime())
            .map(|bridge| Self { bridge })
            .map_err(error_js)
    }

    pub fn join(&mut self, owner_id: String, trusted_session: JsValue, writable: bool) -> JsValue {
        let session = value_from_js(trusted_session);
        response(session.and_then(|session| self.bridge.join(owner_id, session, writable)))
    }

    pub fn subscribe(&mut self, owner_id: String, writable: bool) -> JsValue {
        response(self.bridge.subscribe(&owner_id, writable))
    }

    pub fn patch_connection(
        &self,
        handle: String,
        owner_id: String,
        patch: JsValue,
    ) -> JsValue {
        response(value_from_js(patch).and_then(|patch| {
            self.bridge
                .runtime
                .patch_connection(self.bridge.participant(&handle)?, &owner_id, &patch)
                .map_err(Into::into)
        }))
    }

    pub fn patch_shared(&self, patch: JsValue) -> JsValue {
        response(
            value_from_js(patch)
                .and_then(|patch| self.bridge.runtime.patch_shared(&patch).map_err(Into::into)),
        )
    }

    pub fn patch_shared_from_participant(
        &self,
        handle: String,
        owner_id: String,
        patch: JsValue,
    ) -> JsValue {
        response(value_from_js(patch).and_then(|patch| {
            self.bridge
                .runtime
                .patch_shared_from_participant(
                    self.bridge.participant(&handle)?,
                    &owner_id,
                    &patch,
                )
                .map_err(Into::into)
        }))
    }

    pub fn patch_shared_from_subscription(
        &self,
        handle: String,
        owner_id: String,
        patch: JsValue,
    ) -> JsValue {
        response(value_from_js(patch).and_then(|patch| {
            self.bridge
                .runtime
                .patch_shared_from_subscription(
                    self.bridge.subscription(&handle)?,
                    &owner_id,
                    &patch,
                )
                .map_err(Into::into)
        }))
    }

    pub fn refresh_and_renew(
        &self,
        handle: String,
        owner_id: String,
        trusted_session: JsValue,
    ) -> JsValue {
        response(value_from_js(trusted_session).and_then(|session| {
            self.bridge
                .runtime
                .refresh_and_renew(
                    self.bridge.participant(&handle)?,
                    &owner_id,
                    &session,
                )
                .map(|(change, lease)| Refreshed {
                    change,
                    lease: lease.into(),
                })
                .map_err(Into::into)
        }))
    }

    pub fn renew(&self, handle: String, owner_id: String) -> JsValue {
        response(
            self.bridge
                .participant(&handle)
                .and_then(|participant| {
                    self.bridge
                        .runtime
                        .renew(participant, &owner_id)
                        .map(LeaseValue::from)
                        .map_err(Into::into)
                }),
        )
    }

    pub fn renew_subscription(&self, handle: String, owner_id: String) -> JsValue {
        response(
            self.bridge
                .subscription(&handle)
                .and_then(|subscription| {
                    self.bridge
                        .runtime
                        .renew_subscription(subscription, &owner_id)
                        .map(LeaseValue::from)
                        .map_err(Into::into)
                }),
        )
    }

    pub fn resubscribe(&mut self, handle: String, owner_id: String) -> JsValue {
        let result = self
            .bridge
            .subscription(&handle)
            .cloned()
            .and_then(|subscription| {
                self.bridge
                    .runtime
                    .resubscribe(&subscription, &owner_id)
                    .map_err(Into::into)
            });
        response(result.map(|subscribed| {
            let connection_id = subscribed.subscription.connection_id().to_string();
            self.bridge
                .subscriptions
                .insert(handle.clone(), subscribed.subscription);
            Connected {
                connection_id,
                handle,
                snapshot: subscribed.snapshot,
            }
        }))
    }

    pub fn poll(&self, handle: String) -> JsValue {
        response(
            self.bridge
                .subscription(&handle)
                .and_then(|subscription| {
                    self.bridge.runtime.poll(subscription).map_err(Into::into)
                }),
        )
    }

    pub fn leave(&mut self, handle: String) -> JsValue {
        let result = self
            .bridge
            .participant(&handle)
            .cloned()
            .and_then(|participant| self.bridge.runtime.leave(&participant).map_err(Into::into));
        if result.is_ok() {
            self.bridge.participants.remove(&handle);
            self.bridge.subscriptions.remove(&handle);
        }
        response(result)
    }

    pub fn unsubscribe(&mut self, handle: String) -> JsValue {
        let result = self
            .bridge
            .subscription(&handle)
            .cloned()
            .and_then(|subscription| {
                self.bridge
                    .runtime
                    .unsubscribe(&subscription)
                    .map_err(Into::into)
            });
        if result.is_ok() {
            self.bridge.subscriptions.remove(&handle);
        }
        response(result.map(|_| ()))
    }

    pub fn expire(&mut self) -> JsValue {
        response(self.bridge.expire())
    }

    pub fn snapshot(&self) -> JsValue {
        response(self.bridge.runtime.snapshot().map_err(Into::into))
    }

    pub fn close(&mut self) -> JsValue {
        let result = self.bridge.runtime.close().map_err(Into::into);
        if result.is_ok() {
            self.bridge.participants.clear();
            self.bridge.subscriptions.clear();
        }
        response(result)
    }
}

fn value_from_js(value: JsValue) -> Result<Value, BridgeError> {
    if value.is_null() || value.is_undefined() {
        Ok(Value::Null)
    } else {
        serde_wasm_bindgen::from_value(value)
            .map_err(|error| BridgeError::input("invalid_json", error.to_string()))
    }
}

fn from_js<T: for<'de> Deserialize<'de>>(value: JsValue, code: &str) -> Result<T, JsValue> {
    serde_wasm_bindgen::from_value(value)
        .map_err(|error| error_js(BridgeError::input(code, error.to_string())))
}

fn response<T: Serialize>(result: Result<T, BridgeError>) -> JsValue {
    let value = match result {
        Ok(value) => Response::Ok { ok: true, value },
        Err(error) => Response::<T>::Err { ok: false, error },
    };
    to_js(&value)
}

fn error_js(error: BridgeError) -> JsValue {
    to_js(&error)
}

fn to_js(value: &impl Serialize) -> JsValue {
    let json = serde_json::to_string(value).expect("database runtime response should serialize");
    js_sys::JSON::parse(&json).expect("database runtime response should be valid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyre::ephemeral::{FieldContract, InitialValue, StateContract, ValueSchema, ValueType};

    #[derive(Default)]
    struct TestEnvironment {
        millis: AtomicU64,
        ids: AtomicU64,
    }

    impl RuntimeEnvironment for TestEnvironment {
        fn now(&self) -> RuntimeTime {
            RuntimeTime {
                monotonic_millis: self.millis.load(Ordering::SeqCst),
                unix_seconds: 0,
            }
        }

        fn new_id(&self) -> Result<String, String> {
            Ok(format!("id-{}", self.ids.fetch_add(1, Ordering::SeqCst)))
        }
    }

    fn contract() -> Contract {
        Contract {
            connection: Some(StateContract {
                fields: [(
                    "cursor".to_string(),
                    FieldContract {
                        schema: ValueSchema {
                            type_: ValueType::String,
                            nullable: true,
                        },
                        writable: true,
                        default: None,
                        derived_from: None,
                    },
                )]
                .into_iter()
                .collect(),
            }),
            shared: Some(StateContract {
                fields: [(
                    "count".to_string(),
                    FieldContract {
                        schema: ValueSchema {
                            type_: ValueType::Int,
                            nullable: false,
                        },
                        writable: true,
                        default: Some(InitialValue::Value(Value::from(0))),
                        derived_from: None,
                    },
                )]
                .into_iter()
                .collect(),
            }),
            types: BTreeMap::new(),
        }
    }

    #[test]
    fn private_handles_drive_runtime_validation_and_are_retired() {
        let mut bridge =
            RuntimeBridge::new("db".to_string(), contract(), RuntimeConfig::default()).unwrap();
        let joined = bridge
            .join("owner".to_string(), serde_json::json!({}), true)
            .unwrap();
        let connection_id = joined.connection_id;
        let handle = joined.handle;
        assert_ne!(connection_id, handle);
        bridge
            .runtime
            .patch_connection(
                bridge.participant(&handle).unwrap(),
                "owner",
                &serde_json::json!({"cursor": "x"}),
            )
            .unwrap();
        let participant = bridge.participant(&handle).unwrap().clone();
        bridge.runtime.leave(&participant).unwrap();
        bridge.participants.remove(&handle);
        bridge.subscriptions.remove(&handle);
        assert_eq!(
            bridge.participant(&handle).unwrap_err().code,
            "unknown_connection"
        );
        assert_eq!(
            bridge.subscription(&handle).unwrap_err().code,
            "unknown_subscription"
        );
    }

    #[test]
    fn visible_ids_unknown_handles_and_subscription_handles_cannot_resolve_participants() {
        let mut bridge =
            RuntimeBridge::new("db".to_string(), contract(), RuntimeConfig::default()).unwrap();
        let joined = bridge
            .join("same-owner".to_string(), serde_json::json!({}), true)
            .unwrap();
        let subscribed = bridge.subscribe("same-owner", false).unwrap();

        assert_eq!(
            bridge.participant(&joined.connection_id).unwrap_err().code,
            "unknown_connection"
        );
        assert_eq!(
            bridge.participant(&subscribed.handle).unwrap_err().code,
            "unknown_connection"
        );
        assert_eq!(
            bridge.subscription("unknown-handle").unwrap_err().code,
            "unknown_subscription"
        );
        assert!(bridge.participant(&joined.handle).is_ok());
        assert!(bridge.subscription(&joined.handle).is_ok());
    }

    #[test]
    fn runtime_errors_have_stable_serializable_codes() {
        let error = BridgeError::from(RuntimeError::OwnerMismatch);
        assert_eq!(error.code, "owner_mismatch");
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            serde_json::json!({
                "code": "owner_mismatch",
                "message": "trusted owner does not own the connection"
            })
        );
    }

    #[test]
    fn joined_subscription_cannot_be_unsubscribed_and_keeps_its_handle() {
        let mut bridge =
            RuntimeBridge::new("db".to_string(), contract(), RuntimeConfig::default()).unwrap();
        let joined = bridge
            .join("owner".to_string(), serde_json::json!({}), true)
            .unwrap();
        let error = bridge
            .runtime
            .unsubscribe(bridge.subscription(&joined.handle).unwrap())
            .unwrap_err();
        let error = BridgeError::from(error);

        assert_eq!(error.code, "unknown_connection");
        assert!(bridge.participant(&joined.handle).is_ok());
        assert!(bridge.subscription(&joined.handle).is_ok());
    }

    #[test]
    fn expiry_discards_participant_and_subscription_handles() {
        let environment = Arc::new(TestEnvironment::default());
        let mut config = RuntimeConfig::with_environment(environment.clone());
        config.lease_duration = Duration::from_millis(10);
        let mut bridge = RuntimeBridge::new("db".to_string(), contract(), config).unwrap();
        let joined = bridge
            .join("participant".to_string(), serde_json::json!({}), true)
            .unwrap();
        let subscribed = bridge.subscribe("subscriber", false).unwrap();

        environment.millis.store(10, Ordering::SeqCst);
        let expired = bridge.expire().unwrap();
        assert_eq!(expired.connection_ids, [joined.connection_id.as_str()]);
        assert_eq!(
            expired.subscription_ids,
            [subscribed.connection_id.as_str()]
        );
        assert!(!bridge.participants.contains_key(&joined.handle));
        assert!(!bridge.subscriptions.contains_key(&joined.handle));
        assert!(!bridge.subscriptions.contains_key(&subscribed.handle));
    }

    #[test]
    fn monotonic_elapsed_clamps_backward_or_temporarily_unavailable_sources() {
        assert_eq!(monotonic_elapsed(100, Some(125), 20), 25);
        assert_eq!(monotonic_elapsed(100, Some(110), 25), 25);
        assert_eq!(monotonic_elapsed(100, None, 25), 25);
    }
}
