use pyre::{ast, parser};

#[test]
fn parses_ordered_heterogeneous_transaction_steps() {
    let source = r#"
transaction AcceptInvite($inviteId: Int, $workspaceId: Int, $userId: Int) {
    update acceptedInvite: invite {
        @where { id == $inviteId && acceptedAt == null }
        acceptedAt = now()
    }

    insert membership: workspaceMember {
        workspaceId = $workspaceId
        userId = $userId
    }

    delete pendingRequest: joinRequest {
        @where { workspaceId == $workspaceId && userId == $userId }
    }
}
"#;
    let parsed = parser::parse_query("transaction.pyre", source).expect("transaction parses");
    let ast::QueryDef::Query(query) = &parsed.queries[0] else {
        panic!("expected transaction query");
    };

    assert_eq!(query.operation, ast::QueryOperation::Transaction);
    let steps = query
        .fields
        .iter()
        .filter_map(|field| match field {
            ast::TopLevelQueryField::Field(field) => {
                Some((field.operation.clone(), ast::get_aliased_name(field)))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        steps,
        vec![
            (
                Some(ast::QueryOperation::Update),
                "acceptedInvite".to_string()
            ),
            (Some(ast::QueryOperation::Insert), "membership".to_string()),
            (
                Some(ast::QueryOperation::Delete),
                "pendingRequest".to_string()
            ),
        ]
    );
}

#[test]
fn rejects_query_steps_inside_transactions() {
    let source = r#"
transaction Invalid {
    query notes: note { id }
}
"#;
    assert!(parser::parse_query("transaction.pyre", source).is_err());
}
