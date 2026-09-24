use pyre::generate::typealias::{self, FieldType, TypeFormatter};
use pyre::{ast, parser, typecheck};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

fn fixture() -> (typecheck::Context, ast::QueryList) {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        r#"
@syncable(false)
record Entity {
    @public
    key Id.Uuid @id
    children @link(key, Child.ownerKey)
}
record Legacy {
    @public
    key Id.Int @id
}
record Child {
    @public
    key Id.Uuid @id
    ownerKey Entity.key
    legacyKey Legacy.key?
    active Json<List<Entity.key>>
    byName Json<Dict<Entity.key>>
    enabled Bool
    created DateTime?
    owner @link(ownerKey, Entity.key)
}
"#,
        &mut schema,
    )
    .expect("schema parses");
    let context = typecheck::check_schema(&ast::Database {
        schemas: vec![schema],
    })
    .expect("schema typechecks");
    let queries = parser::parse_query(
        "query.pyre",
        r#"
query Results {
    entities: entity {
        identity: key
        children {
            *
            uuidOwner: ownerKey
            owner { key }
        }
    }
}
"#,
    )
    .expect("query parses");
    typecheck::check_queries(&queries, &context).expect("query typechecks");
    (context, queries)
}

#[test]
fn result_formatter_receives_resolved_types_and_opaque_relationship_names() {
    let (context, queries) = fixture();
    let query = queries
        .queries
        .iter()
        .find_map(|query| match query {
            ast::QueryDef::Query(query) => Some(query),
            _ => None,
        })
        .unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let observed = seen.clone();
    let formatter = TypeFormatter {
        to_comment: Box::new(|_| String::new()),
        to_type_def_start: Box::new(|_| String::new()),
        to_field: Box::new(move |name, type_, metadata| {
            observed.borrow_mut().push(name.to_string());
            match name {
                "identity" => assert!(
                    matches!(type_, FieldType::Column(ast::ColumnType::IdUuid { table }) if table == "Entity")
                ),
                "uuidOwner" => assert!(
                    matches!(type_, FieldType::Column(ast::ColumnType::ForeignKey {
                    table, field, serialization_type: Some(ast::ConcreteSerializationType::IdUuid), ..
                }) if table == "Entity" && field == "key")
                ),
                "legacyKey" => {
                    assert!(metadata.is_optional);
                    assert!(matches!(
                        type_,
                        FieldType::Column(ast::ColumnType::ForeignKey {
                            serialization_type: Some(ast::ConcreteSerializationType::IdInt),
                            ..
                        })
                    ));
                }
                "active" => {
                    let FieldType::Column(ast::ColumnType::JsonTyped(inner)) = type_ else {
                        panic!("typed JSON")
                    };
                    let ast::ColumnType::List(element) = inner.as_ref() else {
                        panic!("list")
                    };
                    assert!(matches!(
                        element.as_ref(),
                        ast::ColumnType::ForeignKey {
                            serialization_type: Some(ast::ConcreteSerializationType::IdUuid),
                            ..
                        }
                    ));
                }
                "owner" => {
                    assert!(
                        matches!(type_, FieldType::Relationship(name) if name == "Entities_Children_Owner")
                    );
                    assert!(metadata.is_optional);
                    assert!(!metadata.is_array_relationship);
                }
                "children" | "entities" => {
                    assert!(matches!(type_, FieldType::Relationship(_)));
                    assert!(metadata.is_array_relationship);
                    assert!(!metadata.is_optional);
                }
                _ => {}
            }
            String::new()
        }),
        to_type_def_end: Box::new(String::new),
        to_field_separator: Box::new(|_| String::new()),
    };
    typealias::return_data_aliases(&context, query, &mut String::new(), &formatter);
    let seen = seen.borrow();
    for name in [
        "identity",
        "uuidOwner",
        "legacyKey",
        "active",
        "owner",
        "children",
        "entities",
    ] {
        assert!(seen.iter().any(|field| field == name), "missing {name}");
    }
    assert!(
        !seen.iter().any(|field| field == "ownerKey"),
        "wildcards must respect explicit aliases"
    );
}

#[test]
fn all_result_renderers_preserve_identity_types_and_relationships() {
    let (context, queries) = fixture();
    let info = typecheck::check_queries(&queries, &context).unwrap();
    let mut files = Vec::new();
    pyre::generate::write_queries(&context, &queries, &info, &mut files);
    let content = |suffix: &str| -> &str {
        &files
            .iter()
            .find(|file| file.path.ends_with(Path::new(suffix)))
            .unwrap_or_else(|| panic!("missing {suffix}"))
            .contents
    };
    for (suffix, expected) in [
        (
            "rust/server.rs",
            vec![
                "pub identity: String",
                "pub uuid_owner: String",
                "pub legacy_key: Option<i64>",
                "pub active: Vec<String>",
                "pub by_name: std::collections::HashMap<String, String>",
                "pub owner: Option<EntitiesChildrenOwner>",
                "pub children: Vec<EntitiesChildren>",
                "pub entities: Vec<Entities>",
                "pub created: Option<DateTime>",
                "pub enabled: bool",
            ],
        ),
        (
            "queries/metadata/results.ts",
            vec![
                "identity: z.string()",
                "uuidOwner: z.string()",
                "legacyKey: z.number().nullable()",
                "active: z.array(z.string())",
                "byName: z.record(z.string(), z.string())",
                "owner: Entities_Children_Owner.nullable()",
                "children: Entities_Children.array()",
                "entities: Entities.array()",
                "created: CoercedDate.nullable()",
                "enabled: CoercedBool",
            ],
        ),
        (
            "Query/Results.elm",
            vec![
                "identity : Db.Id.Entity",
                "uuidOwner : Db.Id.Entity",
                "legacyKey : Maybe Db.Id.Legacy",
                "active : List Db.Id.Entity",
                "byName : Dict String Db.Id.Entity",
                "owner : Maybe Entities_Children_Owner",
                "children : List Entities_Children",
                "entities : List Entities",
                "created : Maybe Time.Posix",
                "enabled : Bool",
                "Db.Decode.andField \"uuidOwner\" Db.Id.decodeUuid",
                "Db.Decode.andField \"legacyKey\" (Decode.nullable Db.Id.decodeInt)",
                "Db.Decode.andField \"active\" (Decode.list Db.Id.decodeUuid)",
            ],
        ),
    ] {
        let generated = content(suffix);
        for expected in expected {
            assert!(
                generated.contains(expected),
                "{suffix}: missing {expected}\n{generated}"
            );
        }
    }
}
