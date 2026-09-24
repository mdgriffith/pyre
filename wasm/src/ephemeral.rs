use pyre::ephemeral::{Contract, ErrorCode, ValidationError};
use serde::Serialize;
use serde_json::Value;
use wasm_bindgen::JsValue;

#[derive(Serialize)]
#[serde(untagged)]
enum Response<T> {
    Ok { ok: bool, value: T },
    Err { ok: bool, errors: Vec<ValidationError> },
}

pub fn initialize(
    contract: JsValue,
    state: String,
    trusted_session: JsValue,
    now_seconds: i64,
) -> JsValue {
    let contract: Contract = match serde_wasm_bindgen::from_value(contract) {
        Ok(contract) => contract,
        Err(error) => return invalid_contract(error.to_string()),
    };
    let session: Value = if trusted_session.is_null() || trusted_session.is_undefined() {
        Value::Null
    } else {
        match serde_wasm_bindgen::from_value(trusted_session) {
            Ok(session) => session,
            Err(error) => return invalid_contract(error.to_string()),
        }
    };
    let result = match state.as_str() {
        "Connection" => contract.initialize_connection(&session, now_seconds),
        "Shared" => contract.initialize_shared(now_seconds),
        _ => contract.validate_complete(&state, &Value::Null),
    };
    response(result)
}

pub fn apply_patch(
    contract: JsValue,
    state: String,
    current: JsValue,
    patch: JsValue,
) -> JsValue {
    let contract: Contract = match serde_wasm_bindgen::from_value(contract) {
        Ok(contract) => contract,
        Err(error) => return invalid_contract(error.to_string()),
    };
    let current = match serde_wasm_bindgen::from_value(current) {
        Ok(current) => current,
        Err(error) => return invalid_contract(error.to_string()),
    };
    let patch = match serde_wasm_bindgen::from_value(patch) {
        Ok(patch) => patch,
        Err(error) => return invalid_contract(error.to_string()),
    };
    response(contract.apply_patch(&state, &current, &patch))
}

fn response<T: Serialize>(result: Result<T, Vec<ValidationError>>) -> JsValue {
    let response = match result {
        Ok(value) => Response::Ok { ok: true, value },
        Err(errors) => Response::<T>::Err { ok: false, errors },
    };
    to_js(&response)
}

fn invalid_contract(message: String) -> JsValue {
    to_js(&Response::<Value>::Err {
        ok: false,
        errors: vec![ValidationError {
            code: ErrorCode::InvalidContract,
            path: Vec::new(),
            message,
        }],
    })
}

fn to_js(value: &impl Serialize) -> JsValue {
    let json = serde_json::to_string(value).expect("ephemeral response should serialize");
    js_sys::JSON::parse(&json).expect("serialized ephemeral response should be valid JSON")
}
