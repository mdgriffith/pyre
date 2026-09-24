use pyre::{ast, format, generate, parser, typecheck};

fn parse(source: &str) -> ast::Schema {
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", source, &mut schema).expect("schema should parse");
    schema
}

fn check(source: &str) -> Result<typecheck::Context, Vec<pyre::error::Error>> {
    typecheck::check_schema(&ast::Database {
        schemas: vec![parse(source)],
    })
}

fn messages(source: &str) -> Vec<String> {
    errors(source)
        .into_iter()
        .map(|error| pyre::error::to_error_description(&error, false))
        .collect()
}

fn errors(source: &str) -> Vec<pyre::error::Error> {
    check(source).expect_err("schema should fail typechecking")
}

#[test]
fn parses_state_as_distinct_ast_with_writable_and_derived_fields() {
    let schema = parse(
        r#"session {
    userId Int
}

state Connection {
    cursor Json<List<String>>?
    userId Int = Session.userId
}
"#,
    );

    let state = schema.files[0]
        .definitions
        .iter()
        .find_map(|definition| match definition {
            ast::Definition::State { name, fields, .. } => Some((name, fields)),
            _ => None,
        })
        .expect("state definition");
    assert_eq!(state.0, "Connection");
    assert!(matches!(state.1[0], ast::StateField::Writable(_)));
    assert!(matches!(
        &state.1[1],
        ast::StateField::Derived { column, source }
            if column.name == "userId"
                && source.root == "Session"
                && source.path == ["userId"]
    ));
}

#[test]
fn parser_accepts_arbitrary_state_names_for_typechecking() {
    let schema = parse(
        r#"state Presence {
    active Bool @default(False)
}
"#,
    );
    assert!(schema.files[0].definitions.iter().any(
        |definition| matches!(definition, ast::Definition::State { name, .. } if name == "Presence")
    ));
}

#[test]
fn formats_state_declarations_and_derivations_idempotently() {
    let source = r#"session {
 userId Int
}
state Connection {
// current cursor
cursor Json<List<String>>?
userId Int=Session.userId // trusted
}
state Shared {
slide Int @default(0)
}
"#;
    let mut schema = parse(source);
    format::schema(&mut schema);
    let formatted = generate::to_string::schema_to_string("", &schema);

    assert!(formatted.contains("state Connection {\n    // current cursor\n"));
    assert!(formatted.contains("userId Int = Session.userId // trusted"));
    assert!(formatted.contains("state Shared {\n    slide Int @default(0)\n}"));

    let mut reparsed = parse(&formatted);
    format::schema(&mut reparsed);
    assert_eq!(
        formatted,
        generate::to_string::schema_to_string("", &reparsed)
    );
}

#[test]
fn valid_state_types_defaults_and_derivations_typecheck_without_tables() {
    let context = check(
        r#"type Presence
   = Active
   | Away { since DateTime }

session {
    userId Int
    presence Presence?
}

state Connection {
    userId   Int       = Session.userId
    presence Presence? = Session.presence
    cursor   Json<Dict<List<String>>>?
    label    String    @default("online")
}

state Shared {
    slide     Int       @default(0)
    presence  Presence  @default(Active)
    snapshot  Json<Presence> @default(Active)
    selection Json<List<String>>?
}
"#,
    )
    .expect("valid state schema");

    assert!(context.tables.is_empty(), "state must not become a table");
}

#[test]
fn nullable_writable_field_without_default_is_valid_null_initialization_contract() {
    check(
        r#"state Shared {
    selection String?
}
"#,
    )
    .expect("nullable writable state fields initialize to null");
}

#[test]
fn rejects_unknown_and_duplicate_reserved_state_declarations() {
    let unknown = messages(
        r#"state Presence {
    active Bool @default(False)
}
"#,
    );
    assert!(unknown
        .iter()
        .any(|message| message.contains("Only Connection and Shared")));

    let duplicate = messages(
        r#"state Shared {
    value Int @default(0)
}

state Shared {
    other Int @default(0)
}
"#,
    );
    assert!(duplicate
        .iter()
        .any(|message| message.contains("at most one state Shared")));
}

#[test]
fn rejects_uninitialized_non_nullable_writable_fields_and_invalid_defaults() {
    let uninitialized = messages(
        r#"state Connection {
    cursor String
}
"#,
    );
    assert!(uninitialized
        .iter()
        .any(|message| message.contains("must be nullable or have an explicit default")));

    let invalid_default = errors(
        r#"state Shared {
    count Int @default("many")
}
"#,
    );
    assert!(invalid_default.iter().any(|error| matches!(
        error.error_type,
        pyre::error::ErrorType::InvalidColumnDefault { .. }
    )));

    let incomplete_custom_default = errors(
        r#"type Presence
   = Active
   | Away { since DateTime }

state Shared {
    presence Presence @default(Away)
}
"#,
    );
    assert!(incomplete_custom_default.iter().any(|error| matches!(
        error.error_type,
        pyre::error::ErrorType::InvalidColumnDefault { .. }
    )));

    let invalid_date = errors(
        r#"state Shared {
    day Date @default("24/09/2026")
}
"#,
    );
    assert!(invalid_date.iter().any(|error| matches!(
        error.error_type,
        pyre::error::ErrorType::InvalidColumnDefault { .. }
    )));
}

#[test]
fn rejects_record_persistence_features_in_state() {
    for (source, expected) in [
        (
            r#"state Shared {
    id Int @id @default(0)
}
"#,
            "persistence directives",
        ),
        (
            r#"state Shared {
    @tablename("shared")
    value Int @default(0)
}
"#,
            "persistence directives",
        ),
        (
            r#"state Shared {
    owner @link(User.id)
}
"#,
            "Links are not allowed",
        ),
    ] {
        assert!(messages(source)
            .iter()
            .any(|message| message.contains(expected)));
    }
}

#[test]
fn rejects_invalid_state_derivations() {
    let cases = [
        (
            r#"session { userId Int
}
state Shared {
    userId Int = Session.userId
}
"#,
            "only allowed in state Connection",
        ),
        (
            r#"session { userId Int
}
state Connection {
    userId Int = User.userId
}
"#,
            "directly reference one Session field",
        ),
        (
            r#"session { userId Int
}
state Connection {
    userId Int = Session.user.profile
}
"#,
            "directly reference one Session field",
        ),
        (
            r#"session { userId Int
}
state Connection {
    ownerId Int = Session.ownerId
}
"#,
            "Session has no field named 'ownerId'",
        ),
        (
            r#"session { userId Int
}
state Connection {
    userId String = Session.userId
}
"#,
            "same type and nullability",
        ),
        (
            r#"session { userId Int?
}
state Connection {
    userId Int = Session.userId
}
"#,
            "same type and nullability",
        ),
    ];

    for (source, expected) in cases {
        assert!(
            messages(source)
                .iter()
                .any(|message| message.contains(expected)),
            "expected error containing {expected}"
        );
    }
}

#[test]
fn rejects_duplicate_and_unknown_typed_state_fields() {
    let duplicate = errors(
        r#"state Shared {
    value Int?
    value String?
}
"#,
    );
    assert!(duplicate.iter().any(|error| matches!(
        &error.error_type,
        pyre::error::ErrorType::DuplicateField { field, .. } if field == "value"
    )));

    let unknown_type = errors(
        r#"state Shared {
    value Missing?
}
"#,
    );
    assert!(unknown_type.iter().any(|error| matches!(
        &error.error_type,
        pyre::error::ErrorType::UnknownType { found, .. } if found == "Missing"
    )));
}

#[test]
fn rejects_custom_types_that_collide_with_generated_state_declarations() {
    for name in [
        "Connection",
        "ConnectionPatch",
        "Shared",
        "SharedPatch",
        "StateTypes",
        "StateName",
        "PatchField",
        "StateMetadata",
    ] {
        let source =
            format!("type {name}\n   = Value\n\nstate Shared {{\n    value Int @default(0)\n}}\n");
        assert!(
            messages(&source).iter().any(|message| message
                .contains(&format!("Type '{name}' emits the generated identifier"))),
            "expected a generated declaration collision for {name}"
        );
    }

    for name in ["Connection_Patch", "Patch_Field", "State_Metadata"] {
        let source =
            format!("type {name}\n   = Value\n\nstate Shared {{\n    value Int @default(0)\n}}\n");
        assert!(
            messages(&source)
                .iter()
                .any(|message| message.contains("conflicts with an ephemeral state declaration")),
            "expected normalized generated declaration collision for {name}"
        );
    }

    let collapsed = messages(
        r#"type User_Profile
   = First

type User__Profile
   = Second

state Shared {
    value Int @default(0)
}
"#,
    );
    assert!(collapsed.iter().any(|message| message.contains(
        "Types 'User_Profile' and 'User__Profile' both emit the generated Rust identifier 'UserProfile'"
    )));

    check("type StateTypes\n   = Value\n")
        .expect("reserved state names remain legal without state generation");
}
