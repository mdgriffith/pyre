use pyre::{
    ephemeral::{Contract, ValidationError},
    server::runtime::{
        Change, DatabaseRuntime, JoinEvidence, Lease, Participant, RuntimeConfig,
        RuntimeEnvironment, RuntimeError, RuntimeTime, SharedWritePolicy, Snapshot, Subscription,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
        config
    }
}

struct WasmEnvironment {
    started_millis: u64,
    last_millis: AtomicU64,
}

impl Default for WasmEnvironment {
    fn default() -> Self {
        Self {
            started_millis: wall_clock_millis(),
            last_millis: AtomicU64::new(0),
        }
    }
}

impl RuntimeEnvironment for WasmEnvironment {
    fn now(&self) -> RuntimeTime {
        let elapsed = wall_clock_millis().saturating_sub(self.started_millis);
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
        let joined = self.runtime.join(JoinEvidence {
            owner_id,
            trusted_session,
            writable,
        })?;
        let connection_id = joined.participant.connection_id().to_string();
        self.participants
            .insert(connection_id.clone(), joined.participant);
        self.subscriptions
            .insert(connection_id.clone(), joined.subscription);
        Ok(Connected {
            connection_id,
            snapshot: joined.snapshot,
        })
    }

    fn subscribe(&mut self, owner_id: &str, writable: bool) -> Result<Connected, BridgeError> {
        let subscribed = self.runtime.subscribe(owner_id, writable)?;
        let connection_id = subscribed.subscription.connection_id().to_string();
        self.subscriptions
            .insert(connection_id.clone(), subscribed.subscription);
        Ok(Connected {
            connection_id,
            snapshot: subscribed.snapshot,
        })
    }

    fn participant(&self, connection_id: &str) -> Result<&Participant, BridgeError> {
        self.participants
            .get(connection_id)
            .ok_or_else(|| BridgeError::from(RuntimeError::UnknownConnection))
    }

    fn subscription(&self, connection_id: &str) -> Result<&Subscription, BridgeError> {
        self.subscriptions
            .get(connection_id)
            .ok_or_else(|| BridgeError::from(RuntimeError::UnknownSubscription))
    }

    fn expire(&mut self) -> Result<Expired, BridgeError> {
        let expired = self.runtime.expire_leases_detailed()?;
        for id in &expired.connection_ids {
            self.participants.remove(id);
            self.subscriptions.remove(id);
        }
        for id in &expired.subscription_ids {
            self.subscriptions.remove(id);
        }
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
        connection_id: String,
        owner_id: String,
        patch: JsValue,
    ) -> JsValue {
        response(value_from_js(patch).and_then(|patch| {
            self.bridge
                .runtime
                .patch_connection(self.bridge.participant(&connection_id)?, &owner_id, &patch)
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
        connection_id: String,
        owner_id: String,
        patch: JsValue,
    ) -> JsValue {
        response(value_from_js(patch).and_then(|patch| {
            self.bridge
                .runtime
                .patch_shared_from_participant(
                    self.bridge.participant(&connection_id)?,
                    &owner_id,
                    &patch,
                )
                .map_err(Into::into)
        }))
    }

    pub fn patch_shared_from_subscription(
        &self,
        connection_id: String,
        owner_id: String,
        patch: JsValue,
    ) -> JsValue {
        response(value_from_js(patch).and_then(|patch| {
            self.bridge
                .runtime
                .patch_shared_from_subscription(
                    self.bridge.subscription(&connection_id)?,
                    &owner_id,
                    &patch,
                )
                .map_err(Into::into)
        }))
    }

    pub fn refresh_and_renew(
        &self,
        connection_id: String,
        owner_id: String,
        trusted_session: JsValue,
    ) -> JsValue {
        response(value_from_js(trusted_session).and_then(|session| {
            self.bridge
                .runtime
                .refresh_and_renew(
                    self.bridge.participant(&connection_id)?,
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

    pub fn renew(&self, connection_id: String, owner_id: String) -> JsValue {
        response(
            self.bridge
                .participant(&connection_id)
                .and_then(|participant| {
                    self.bridge
                        .runtime
                        .renew(participant, &owner_id)
                        .map(LeaseValue::from)
                        .map_err(Into::into)
                }),
        )
    }

    pub fn renew_subscription(&self, connection_id: String, owner_id: String) -> JsValue {
        response(
            self.bridge
                .subscription(&connection_id)
                .and_then(|subscription| {
                    self.bridge
                        .runtime
                        .renew_subscription(subscription, &owner_id)
                        .map(LeaseValue::from)
                        .map_err(Into::into)
                }),
        )
    }

    pub fn resubscribe(&mut self, connection_id: String, owner_id: String) -> JsValue {
        let result = self
            .bridge
            .subscription(&connection_id)
            .cloned()
            .and_then(|subscription| {
                self.bridge
                    .runtime
                    .resubscribe(&subscription, &owner_id)
                    .map_err(Into::into)
            });
        response(result.map(|subscribed| {
            self.bridge
                .subscriptions
                .insert(connection_id.clone(), subscribed.subscription);
            Connected {
                connection_id,
                snapshot: subscribed.snapshot,
            }
        }))
    }

    pub fn poll(&self, connection_id: String) -> JsValue {
        response(
            self.bridge
                .subscription(&connection_id)
                .and_then(|subscription| {
                    self.bridge.runtime.poll(subscription).map_err(Into::into)
                }),
        )
    }

    pub fn leave(&mut self, connection_id: String) -> JsValue {
        let result = self
            .bridge
            .participant(&connection_id)
            .cloned()
            .and_then(|participant| self.bridge.runtime.leave(&participant).map_err(Into::into));
        if result.is_ok() {
            self.bridge.participants.remove(&connection_id);
            self.bridge.subscriptions.remove(&connection_id);
        }
        response(result)
    }

    pub fn unsubscribe(&mut self, connection_id: String) -> JsValue {
        let result = self
            .bridge
            .subscription(&connection_id)
            .cloned()
            .and_then(|subscription| {
                self.bridge
                    .runtime
                    .unsubscribe(&subscription)
                    .map_err(Into::into)
            });
        if result.is_ok() {
            self.bridge.subscriptions.remove(&connection_id);
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
        let id = joined.connection_id;
        bridge
            .runtime
            .patch_connection(
                bridge.participant(&id).unwrap(),
                "owner",
                &serde_json::json!({"cursor": "x"}),
            )
            .unwrap();
        let participant = bridge.participant(&id).unwrap().clone();
        bridge.runtime.leave(&participant).unwrap();
        bridge.participants.remove(&id);
        bridge.subscriptions.remove(&id);
        assert_eq!(
            bridge.participant(&id).unwrap_err().code,
            "unknown_connection"
        );
        assert_eq!(
            bridge.subscription(&id).unwrap_err().code,
            "unknown_subscription"
        );
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
        assert!(!bridge.participants.contains_key(&joined.connection_id));
        assert!(!bridge.subscriptions.contains_key(&joined.connection_id));
        assert!(!bridge.subscriptions.contains_key(&subscribed.connection_id));
    }
}
