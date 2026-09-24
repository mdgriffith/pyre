use pyre::ephemeral::{Contract, ErrorCode};
use pyre::server::manifest::Manifest;
use pyre::{ast, generate, parser, typecheck};
use serde_json::{json, Value};

fn context(source: &str) -> typecheck::Context {
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", source, &mut schema).expect("schema should parse");
    typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .expect("schema should typecheck")
}

fn fixture() -> typecheck::Context {
    context(
        r#"type Mood
   = Online
   | Away { since DateTime }

type Node
   = Text { value String }
   | Children { items List<Node> }

session {
    userId Int
    mood Mood?
}

state Connection {
    userId Int = Session.userId
    mood Mood? = Session.mood
    cursor Json<Dict<List<Node?>>>?
    label String @default("online")
}

state Shared {
    count Int @default(0)
    ratio Float @default(1)
    active Bool @default(True)
    started DateTime @default(now)
    day Date @default("2026-09-24")
    owner Id.Int @default(7)
    root Node @default(Text { value = "root" })
    selection Dict<List<Node?>>?
}
"#,
    )
}

#[test]
fn resolves_contract_from_preserved_non_table_state_definitions() {
    let context = fixture();
    assert!(context.tables.is_empty());
    assert_eq!(context.states.len(), 2);

    let contract = Contract::from_context(&context).expect("resolved contract");
    assert!(contract.connection.is_some());
    assert!(contract.shared.is_some());
    assert!(contract.types.contains_key("Mood"));
    assert!(contract.types.contains_key("Node"));
}

#[test]
fn initializes_defaults_nulls_and_trusted_derivations_canonically() {
    let contract = Contract::from_context(&fixture()).unwrap();
    let connection = contract
        .initialize_connection(
            &json!({
                "userId": 42,
                "mood": {"_type": "Away", "since": "2026-09-24T12:00:03+02:00"}
            }),
            1234,
        )
        .unwrap();
    assert_eq!(
        connection,
        json!({
            "cursor": null,
            "label": "online",
            "mood": {"_type": "Away", "since": 1790244003_i64},
            "userId": 42
        })
    );

    let shared = contract.initialize_shared(1234).unwrap();
    assert_eq!(shared["started"], 1234);
    assert_eq!(shared["selection"], Value::Null);
    assert_eq!(shared["owner"], 7);
    assert_eq!(shared["root"], json!({"_type": "Text", "value": "root"}));
    assert_eq!(shared["ratio"].as_f64(), Some(1.0));
}

#[test]
fn validates_recursive_custom_list_dict_and_typed_json_values_strictly() {
    let contract = Contract::from_context(&fixture()).unwrap();
    let current = contract
        .initialize_connection(&json!({"userId": 1, "mood": null}), 0)
        .unwrap();
    let valid = json!({
        "cursor": {
            "left": [
                {"_type": "Text", "value": "a"},
                null,
                {"_type": "Children", "items": [{"_type": "Text", "value": "b"}]}
            ]
        }
    });
    let patched = contract
        .apply_patch("Connection", &current, &valid)
        .unwrap();
    assert!(patched.changed);
    assert_eq!(patched.value["cursor"], valid["cursor"]);

    let errors = contract
        .apply_patch(
            "Connection",
            &current,
            &json!({
                "cursor": {"left": [{"_type": "Text", "value": 3, "extra": true}]}
            }),
        )
        .unwrap_err();
    assert!(errors.iter().any(|error| {
        error.code == ErrorCode::InvalidType && error.path == ["cursor", "left", "0", "value"]
    }));
    assert!(errors.iter().any(|error| {
        error.code == ErrorCode::UnknownField && error.path == ["cursor", "left", "0", "extra"]
    }));

    let errors = contract
        .apply_patch(
            "Connection",
            &current,
            &json!({"cursor": {"left": [{"_type": "Missing"}]}}),
        )
        .unwrap_err();
    assert!(errors
        .iter()
        .any(|error| error.code == ErrorCode::UnknownVariant));

    let errors = contract
        .apply_patch(
            "Connection",
            &current,
            &json!({"cursor": {"left": [{"_type": "Children"}]}}),
        )
        .unwrap_err();
    assert!(errors.iter().any(|error| {
        error.code == ErrorCode::MissingField && error.path == ["cursor", "left", "0", "items"]
    }));
}

#[test]
fn nullable_variant_payloads_must_be_present_but_may_be_null() {
    let contract = Contract::from_context(&context(
        r#"type Choice
   = Selected { note String? }

state Shared {
    choice Choice @default(Selected { note = null })
}
"#,
    ))
    .unwrap();
    let current = contract.initialize_shared(0).unwrap();
    assert_eq!(
        current["choice"],
        json!({"_type": "Selected", "note": null})
    );

    let errors = contract
        .apply_patch(
            "Shared",
            &current,
            &json!({"choice": {"_type": "Selected"}}),
        )
        .unwrap_err();
    assert!(errors.iter().any(|error| {
        error.code == ErrorCode::MissingField && error.path == ["choice", "note"]
    }));
}

#[test]
fn patches_are_atomic_top_level_replacements_and_reject_derived_or_unknown_fields() {
    let contract = Contract::from_context(&fixture()).unwrap();
    let current = contract.initialize_shared(100).unwrap();
    let errors = contract
        .apply_patch(
            "Shared",
            &current,
            &json!({
                "count": 9,
                "root": {"_type": "Text"},
                "unknown": 1
            }),
        )
        .unwrap_err();
    assert!(errors
        .iter()
        .any(|error| error.code == ErrorCode::MissingField));
    assert!(errors
        .iter()
        .any(|error| error.code == ErrorCode::UnknownField));
    assert_eq!(
        current["count"], 0,
        "failed patch cannot alter current value"
    );

    let connection = contract
        .initialize_connection(&json!({"userId": 1, "mood": null}), 0)
        .unwrap();
    let errors = contract
        .apply_patch("Connection", &connection, &json!({"userId": 2}))
        .unwrap_err();
    assert_eq!(errors[0].code, ErrorCode::DerivedField);

    let replaced = contract
        .apply_patch(
            "Shared",
            &current,
            &json!({"root": {"_type": "Children", "items": []}}),
        )
        .unwrap();
    assert_eq!(
        replaced.value["root"],
        json!({"_type": "Children", "items": []})
    );
}

#[test]
fn complete_values_and_patches_normalize_datetime_and_reject_noncanonical_shapes() {
    let contract = Contract::from_context(&fixture()).unwrap();
    let current = contract.initialize_shared(0).unwrap();
    let patched = contract
        .apply_patch(
            "Shared",
            &current,
            &json!({"started": "1970-01-01T00:01:40Z"}),
        )
        .unwrap();
    assert_eq!(patched.value["started"], 100);

    for patch in [
        json!({"active": 1}),
        json!({"day": "24/09/2026"}),
        json!({"root": "Text"}),
        json!({"count": null}),
    ] {
        assert!(contract.apply_patch("Shared", &current, &patch).is_err());
    }

    let mut incomplete = current.clone();
    incomplete.as_object_mut().unwrap().remove("count");
    assert!(contract
        .validate_complete("Shared", &incomplete)
        .unwrap_err()
        .iter()
        .any(|error| error.code == ErrorCode::MissingField));
}

#[test]
fn serialized_contract_has_the_same_results_as_the_native_contract() {
    let native = Contract::from_context(&fixture()).unwrap();
    let wasm_boundary: Contract =
        serde_json::from_value(serde_json::to_value(&native).unwrap()).unwrap();
    let session = json!({"userId": 4, "mood": {"_type": "Online"}});
    assert_eq!(
        native.initialize_connection(&session, 99),
        wasm_boundary.initialize_connection(&session, 99)
    );
    let current = native.initialize_shared(99).unwrap();
    let patch = json!({"count": 2});
    assert_eq!(
        native.apply_patch("Shared", &current, &patch),
        wasm_boundary.apply_patch("Shared", &current, &patch)
    );
}

#[test]
fn validates_uuid_ids_and_resolved_foreign_key_scalars() {
    let contract = Contract::from_context(&context(
        r#"@syncable(false)
record User {
    @public
    id Id.Uuid @id
}

state Shared {
    id Id.Uuid @default("018f6f50-63d8-7ca2-9a8d-f9d05b35d251")
    owner User.id @default("018f6f50-63d8-7ca2-9a8d-f9d05b35d251")
}
"#,
    ))
    .unwrap();
    let current = contract.initialize_shared(0).unwrap();
    assert!(contract
        .apply_patch(
            "Shared",
            &current,
            &json!({"owner": "018f6f50-63d8-7ca2-9a8d-f9d05b35d252"}),
        )
        .is_ok());
    assert!(contract
        .apply_patch("Shared", &current, &json!({"owner": 2}))
        .is_err());
}

#[test]
fn manifest_roundtrips_the_exact_contract_and_old_manifests_remain_loadable() {
    let context = fixture();
    let expected = Contract::from_context(&context).unwrap();
    let mut files = Vec::new();
    generate::manifest::generate_schema(&context, &mut files);
    let generated = files
        .iter()
        .find(|file| file.path.ends_with("manifest.json"))
        .unwrap();
    let manifest: Manifest = serde_json::from_str(&generated.contents).unwrap();
    assert_eq!(manifest.ephemeral, Some(expected));

    let old: Manifest = serde_json::from_value(json!({
        "version": 1,
        "session_schema": {},
        "queries": {}
    }))
    .unwrap();
    assert_eq!(old.ephemeral, None);
    assert!(serde_json::to_value(old)
        .unwrap()
        .get("ephemeral")
        .is_none());
}
