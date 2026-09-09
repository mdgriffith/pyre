use pyre::{ast, error, parser, typecheck};

fn database() -> ast::Database {
    let mut schema = ast::Schema::default();
    parser::run(
        "schema.pyre",
        "record Ab {\n id Int @id\n @allow(*) { True }\n}\nrecord B {\n id Int @id\n @allow(*) { True }\n}\n",
        &mut schema,
    )
    .expect("schema parses");
    ast::Database {
        schemas: vec![schema],
    }
}

#[test]
fn parser_operation_id_collision_is_rejected_in_both_orders() {
    let db = database();
    let mut context = typecheck::check_schema(&db).expect("schema checks");
    context.current_filepath = "queries.pyre".to_string();

    for (source, first, second) in [
        ("query Q { ab { id } }\nquery Qa { b { id } }\n", "Q", "Qa"),
        ("query Qa { b { id } }\nquery Q { ab { id } }\n", "Qa", "Q"),
    ] {
        let queries = parser::parse_query("queries.pyre", source).expect("queries parse");
        let definitions: Vec<_> = queries
            .queries
            .iter()
            .filter_map(|def| match def {
                ast::QueryDef::Query(query) => Some(query),
                _ => None,
            })
            .collect();
        assert_eq!(definitions.len(), 2);
        assert!(!definitions[0].interface_hash.is_empty());
        assert_eq!(definitions[0].interface_hash, definitions[1].interface_hash);
        for query in &definitions {
            let mut errors = Vec::new();
            typecheck::check_query(&context, &mut errors, query);
            assert!(errors.is_empty(), "each query is valid: {errors:?}");
        }
        let errors = typecheck::check_queries(&queries, &context)
            .err()
            .expect("collision rejected");
        assert_eq!(errors.len(), 1, "{errors:?}");
        let diagnostic = &errors[0];
        assert!(matches!(&diagnostic.error_type,
            error::ErrorType::DuplicateOperationId { first_query, second_query, operation_id }
            if first_query == first && second_query == second && operation_id == &definitions[0].interface_hash
        ));
        assert_eq!(
            error::to_error_title(&diagnostic.error_type),
            "Duplicate Operation ID"
        );
        assert_eq!(diagnostic.filepath, "queries.pyre");
        assert_eq!(diagnostic.locations.len(), 2);
        for (location, query) in diagnostic.locations.iter().zip(&definitions) {
            assert_eq!(location.primary.len(), 1);
            assert_eq!(
                location.primary[0].start.line,
                query.start.as_ref().unwrap().line
            );
            assert_eq!(
                location.primary[0].end.line,
                query.end.as_ref().unwrap().line
            );
        }
        let rendered = error::format_error(source, diagnostic, false);
        assert!(rendered.contains(&format!("Queries {first} and {second}")));
        assert!(rendered.contains(&definitions[0].interface_hash));
    }
}

#[test]
fn duplicate_name_with_different_shape_is_rejected() {
    let db = database();
    let context = typecheck::check_schema(&db).expect("schema checks");
    let source = "query Q { ab { id } }\nquery Q { b { id } }\n";
    let queries = parser::parse_query("queries.pyre", source).expect("queries parse");
    let hashes: Vec<_> = queries
        .queries
        .iter()
        .filter_map(|def| match def {
            ast::QueryDef::Query(query) => Some(&query.interface_hash),
            _ => None,
        })
        .collect();
    assert_eq!(hashes.len(), 2);
    assert_ne!(hashes[0], hashes[1]);
    let errors = typecheck::check_queries(&queries, &context)
        .err()
        .expect("duplicate name rejected");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        matches!(&errors[0].error_type, error::ErrorType::DuplicateQueryName { name } if name == "Q")
    );
    assert_eq!(
        error::to_error_title(&errors[0].error_type),
        "Duplicate Query Name"
    );
    assert!(
        error::format_error(source, &errors[0], false).contains("More than one query is named Q")
    );
}

#[test]
fn noncolliding_queries_retain_both_plans() {
    let db = database();
    let context = typecheck::check_schema(&db).expect("schema checks");
    let queries = parser::parse_query(
        "queries.pyre",
        "query Q { ab { id } }\nquery Other { b { id } }\n",
    )
    .expect("queries parse");
    let plans = typecheck::check_queries(&queries, &context).expect("queries check");
    assert_eq!(plans.len(), 2);
    assert!(plans.contains_key("Q"));
    assert!(plans.contains_key("Other"));
}
