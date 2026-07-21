mod newlines;
mod variant_fields;

use pyre::ast;
use pyre::format;
use pyre::generate;
use pyre::parser;

/// Helper function to compare two schemas ignoring location information
fn schemas_equal_ignoring_locations(a: &ast::Schema, b: &ast::Schema) -> bool {
    if a.namespace != b.namespace {
        return false;
    }

    // Compare session
    match (&a.session, &b.session) {
        (None, None) => {}
        (Some(sa), Some(sb)) => {
            if !session_details_equal_ignoring_locations(sa, sb) {
                return false;
            }
        }
        _ => return false,
    }

    // Compare files
    if a.files.len() != b.files.len() {
        return false;
    }

    for (fa, fb) in a.files.iter().zip(b.files.iter()) {
        if fa.path != fb.path {
            return false;
        }
        if !definitions_equal_ignoring_locations(&fa.definitions, &fb.definitions) {
            return false;
        }
    }

    true
}

fn session_details_equal_ignoring_locations(
    a: &ast::SessionDetails,
    b: &ast::SessionDetails,
) -> bool {
    fields_equal_ignoring_locations(&a.fields, &b.fields)
}

fn definitions_equal_ignoring_locations(
    a: &Vec<ast::Definition>,
    b: &Vec<ast::Definition>,
) -> bool {
    // Filter out Lines entries (whitespace) for comparison, similar to queries
    let a_defs: Vec<_> = a
        .iter()
        .filter(|d| !matches!(d, ast::Definition::Lines { .. }))
        .collect();
    let b_defs: Vec<_> = b
        .iter()
        .filter(|d| !matches!(d, ast::Definition::Lines { .. }))
        .collect();

    if a_defs.len() != b_defs.len() {
        eprintln!(
            "Definition counts differ (after filtering Lines): {} vs {}",
            a_defs.len(),
            b_defs.len()
        );
        return false;
    }

    for (i, (da, db)) in a_defs.iter().zip(b_defs.iter()).enumerate() {
        if !definition_equal_ignoring_locations(da, db) {
            eprintln!("Definition {} differs", i);
            return false;
        }
    }

    true
}

fn definition_equal_ignoring_locations(a: &ast::Definition, b: &ast::Definition) -> bool {
    match (a, b) {
        (ast::Definition::Lines { count: ca }, ast::Definition::Lines { count: cb }) => ca == cb,
        (ast::Definition::Comment { text: ta }, ast::Definition::Comment { text: tb }) => ta == tb,
        (
            ast::Definition::Tagged {
                name: na,
                variants: va,
                ..
            },
            ast::Definition::Tagged {
                name: nb,
                variants: vb,
                ..
            },
        ) => {
            if na != nb {
                return false;
            }
            if va.len() != vb.len() {
                return false;
            }
            for (va_item, vb_item) in va.iter().zip(vb.iter()) {
                if !variant_equal_ignoring_locations(va_item, vb_item) {
                    return false;
                }
            }
            true
        }
        (ast::Definition::Session(sa), ast::Definition::Session(sb)) => {
            session_details_equal_ignoring_locations(sa, sb)
        }
        (
            ast::Definition::Record {
                name: na,
                fields: fa,
                ..
            },
            ast::Definition::Record {
                name: nb,
                fields: fb,
                ..
            },
        ) => {
            if na != nb {
                return false;
            }
            fields_equal_ignoring_locations(fa, fb)
        }
        _ => false,
    }
}

fn variant_equal_ignoring_locations(a: &ast::Variant, b: &ast::Variant) -> bool {
    if a.name != b.name {
        return false;
    }

    match (&a.fields, &b.fields) {
        (None, None) => true,
        (Some(fa), Some(fb)) => fields_equal_ignoring_locations(fa, fb),
        _ => false,
    }
}

fn fields_equal_ignoring_locations(a: &Vec<ast::Field>, b: &Vec<ast::Field>) -> bool {
    // Filter out ColumnLines entries (whitespace) for comparison
    let a_fields: Vec<_> = a
        .iter()
        .filter(|f| !matches!(f, ast::Field::ColumnLines { .. }))
        .collect();
    let b_fields: Vec<_> = b
        .iter()
        .filter(|f| !matches!(f, ast::Field::ColumnLines { .. }))
        .collect();

    // Separate columns, links, and other directives
    let mut a_columns = Vec::new();
    let mut a_links = Vec::new();
    let mut a_directives = Vec::new();
    let mut a_comments = Vec::new();

    let mut b_columns = Vec::new();
    let mut b_links = Vec::new();
    let mut b_directives = Vec::new();
    let mut b_comments = Vec::new();

    for f in a_fields.iter() {
        match f {
            ast::Field::Column(c) => a_columns.push(c),
            ast::Field::FieldDirective(ast::FieldDirective::Link(l)) => a_links.push(l),
            ast::Field::FieldDirective(_) => a_directives.push(f),
            ast::Field::ColumnComment { .. } => a_comments.push(f),
            _ => (),
        }
    }

    for f in b_fields.iter() {
        match f {
            ast::Field::Column(c) => b_columns.push(c),
            ast::Field::FieldDirective(ast::FieldDirective::Link(l)) => b_links.push(l),
            ast::Field::FieldDirective(_) => b_directives.push(f),
            ast::Field::ColumnComment { .. } => b_comments.push(f),
            _ => (),
        }
    }

    // Compare columns (order matters)
    if a_columns.len() != b_columns.len() {
        return false;
    }
    for (ca, cb) in a_columns.iter().zip(b_columns.iter()) {
        if !column_equal_ignoring_locations(ca, cb) {
            return false;
        }
    }

    // Compare links semantically (using link_equivalent which ignores link_name)
    // Order doesn't matter for links
    if a_links.len() != b_links.len() {
        return false;
    }
    // Check if all links in a have an equivalent in b
    for link_a in a_links.iter() {
        let mut found = false;
        for link_b in b_links.iter() {
            if ast::link_equivalent(link_a, link_b) {
                found = true;
                break;
            }
        }
        if !found {
            return false;
        }
    }

    // Compare directives ignoring order (tablename, watch, permissions can be in any order)
    if a_directives.len() != b_directives.len() {
        return false;
    }
    // Check if all directives in a have an equivalent in b
    for dir_a in a_directives.iter() {
        let mut found = false;
        for dir_b in b_directives.iter() {
            if field_equal_ignoring_locations(dir_a, dir_b) {
                found = true;
                break;
            }
        }
        if !found {
            return false;
        }
    }

    // Compare comments (order matters, relative to columns)
    if a_comments.len() != b_comments.len() {
        return false;
    }
    for (ca, cb) in a_comments.iter().zip(b_comments.iter()) {
        if !field_equal_ignoring_locations(ca, cb) {
            return false;
        }
    }

    true
}

fn field_equal_ignoring_locations(a: &ast::Field, b: &ast::Field) -> bool {
    match (a, b) {
        (ast::Field::Column(ca), ast::Field::Column(cb)) => column_equal_ignoring_locations(ca, cb),
        (ast::Field::ColumnLines { count: ca }, ast::Field::ColumnLines { count: cb }) => ca == cb,
        (ast::Field::ColumnComment { text: ta }, ast::Field::ColumnComment { text: tb }) => {
            ta == tb
        }
        (ast::Field::FieldDirective(da), ast::Field::FieldDirective(db)) => {
            field_directive_equal_ignoring_locations(da, db)
        }
        _ => false,
    }
}

fn column_equal_ignoring_locations(a: &ast::Column, b: &ast::Column) -> bool {
    a.name == b.name
        && a.type_ == b.type_
        && a.nullable == b.nullable
        && column_directives_equal_ignoring_locations(&a.directives, &b.directives)
}

fn column_directives_equal_ignoring_locations(
    a: &Vec<ast::ColumnDirective>,
    b: &Vec<ast::ColumnDirective>,
) -> bool {
    if a.len() != b.len() {
        return false;
    }

    for (da, db) in a.iter().zip(b.iter()) {
        if !column_directive_equal_ignoring_locations(da, db) {
            return false;
        }
    }

    true
}

fn column_directive_equal_ignoring_locations(
    a: &ast::ColumnDirective,
    b: &ast::ColumnDirective,
) -> bool {
    match (a, b) {
        (ast::ColumnDirective::PrimaryKey, ast::ColumnDirective::PrimaryKey) => true,
        (ast::ColumnDirective::Unique, ast::ColumnDirective::Unique) => true,
        (ast::ColumnDirective::Index, ast::ColumnDirective::Index) => true,
        (
            ast::ColumnDirective::Default {
                id: ida, value: va, ..
            },
            ast::ColumnDirective::Default {
                id: idb, value: vb, ..
            },
        ) => ida == idb && default_value_equal_ignoring_locations(va, vb),
        _ => false,
    }
}

fn default_value_equal_ignoring_locations(a: &ast::DefaultValue, b: &ast::DefaultValue) -> bool {
    match (a, b) {
        (ast::DefaultValue::Now, ast::DefaultValue::Now) => true,
        (ast::DefaultValue::Value(va), ast::DefaultValue::Value(vb)) => {
            query_value_equal_ignoring_locations(va, vb)
        }
        _ => false,
    }
}

fn field_directive_equal_ignoring_locations(
    a: &ast::FieldDirective,
    b: &ast::FieldDirective,
) -> bool {
    match (a, b) {
        (ast::FieldDirective::Watched(wa), ast::FieldDirective::Watched(wb)) => {
            wa.selects == wb.selects
                && wa.inserts == wb.inserts
                && wa.updates == wb.updates
                && wa.deletes == wb.deletes
        }
        (ast::FieldDirective::TableName((_, ta)), ast::FieldDirective::TableName((_, tb))) => {
            ta == tb
        }
        (ast::FieldDirective::Link(la), ast::FieldDirective::Link(lb)) => {
            link_details_equal_ignoring_locations(la, lb)
        }
        (ast::FieldDirective::Index(ia), ast::FieldDirective::Index(ib))
        | (ast::FieldDirective::Unique(ia), ast::FieldDirective::Unique(ib)) => {
            index_directive_equal_ignoring_locations(ia, ib)
        }
        (ast::FieldDirective::Permissions(pa), ast::FieldDirective::Permissions(pb)) => {
            permission_details_equal_ignoring_locations(pa, pb)
        }
        _ => false,
    }
}

fn index_directive_equal_ignoring_locations(
    a: &ast::IndexDirective,
    b: &ast::IndexDirective,
) -> bool {
    if a.columns.len() != b.columns.len() {
        return false;
    }

    if !a
        .columns
        .iter()
        .zip(&b.columns)
        .all(|(ca, cb)| ca.name == cb.name && ca.direction == cb.direction)
    {
        return false;
    }

    match (&a.where_, &b.where_) {
        (Some(wa), Some(wb)) => where_arg_equal_ignoring_locations(wa, wb),
        (None, None) => true,
        _ => false,
    }
}

fn link_details_equal_ignoring_locations(a: &ast::LinkDetails, b: &ast::LinkDetails) -> bool {
    // Use the existing link_equivalent function which compares by local_ids and foreign,
    // ignoring link_name (which can differ for reciprocal links)
    ast::link_equivalent(a, b)
}

fn permission_details_equal_ignoring_locations(
    a: &ast::PermissionDetails,
    b: &ast::PermissionDetails,
) -> bool {
    match (a, b) {
        (ast::PermissionDetails::Public, ast::PermissionDetails::Public) => true,
        (ast::PermissionDetails::Star(wa), ast::PermissionDetails::Star(wb)) => {
            where_arg_equal_ignoring_locations(wa, wb)
        }
        (ast::PermissionDetails::OnOperation(oa), ast::PermissionDetails::OnOperation(ob)) => {
            if oa.len() != ob.len() {
                return false;
            }
            for (oa_item, ob_item) in oa.iter().zip(ob.iter()) {
                if oa_item.operations != ob_item.operations {
                    return false;
                }
                if !where_arg_equal_ignoring_locations(&oa_item.where_, &ob_item.where_) {
                    return false;
                }
            }
            true
        }
        _ => false,
    }
}

fn where_arg_equal_ignoring_locations(a: &ast::WhereArg, b: &ast::WhereArg) -> bool {
    match (a, b) {
        (ast::WhereArg::Exists(pa, ba), ast::WhereArg::Exists(pb, bb)) => {
            pa.iter()
                .map(|(name, _)| name)
                .eq(pb.iter().map(|(name, _)| name))
                && where_arg_equal_ignoring_locations(ba, bb)
        }
        (ast::WhereArg::Column(sa, ca, oa, va, _), ast::WhereArg::Column(sb, cb, ob, vb, _)) => {
            sa == sb && ca == cb && oa == ob && query_value_equal_ignoring_locations(va, vb)
        }
        (ast::WhereArg::And(va), ast::WhereArg::And(vb)) => {
            if va.len() != vb.len() {
                return false;
            }
            for (va_item, vb_item) in va.iter().zip(vb.iter()) {
                if !where_arg_equal_ignoring_locations(va_item, vb_item) {
                    return false;
                }
            }
            true
        }
        (ast::WhereArg::Or(va), ast::WhereArg::Or(vb)) => {
            if va.len() != vb.len() {
                return false;
            }
            for (va_item, vb_item) in va.iter().zip(vb.iter()) {
                if !where_arg_equal_ignoring_locations(va_item, vb_item) {
                    return false;
                }
            }
            true
        }
        _ => false,
    }
}

fn query_value_equal_ignoring_locations(a: &ast::QueryValue, b: &ast::QueryValue) -> bool {
    match (a, b) {
        (ast::QueryValue::Fn(fa), ast::QueryValue::Fn(fb)) => {
            fa.name == fb.name
                && fa.args.len() == fb.args.len()
                && fa
                    .args
                    .iter()
                    .zip(fb.args.iter())
                    .all(|(a, b)| query_value_equal_ignoring_locations(a, b))
        }
        (
            ast::QueryValue::LiteralTypeValue((_, la)),
            ast::QueryValue::LiteralTypeValue((_, lb)),
        ) => {
            la.name == lb.name
                && match (&la.fields, &lb.fields) {
                    (None, None) => true,
                    (Some(fa), Some(fb)) => {
                        fa.len() == fb.len()
                            && fa.iter().zip(fb.iter()).all(|((na, va), (nb, vb))| {
                                na == nb && query_value_equal_ignoring_locations(va, vb)
                            })
                    }
                    _ => false,
                }
        }
        (ast::QueryValue::Variable((_, va)), ast::QueryValue::Variable((_, vb))) => {
            va.name == vb.name && va.session_field == vb.session_field
        }
        (ast::QueryValue::String((_, sa)), ast::QueryValue::String((_, sb))) => sa == sb,
        (ast::QueryValue::Int((_, ia)), ast::QueryValue::Int((_, ib))) => ia == ib,
        (ast::QueryValue::Float((_, fa)), ast::QueryValue::Float((_, fb))) => {
            (fa - fb).abs() < f32::EPSILON
        }
        (ast::QueryValue::Bool((_, ba)), ast::QueryValue::Bool((_, bb))) => ba == bb,
        (ast::QueryValue::Null(_), ast::QueryValue::Null(_)) => true,
        _ => false,
    }
}

/// Helper function to compare two query lists ignoring location information
/// Filters out QueryLines entries as they're just formatting whitespace
fn query_lists_equal_ignoring_locations(a: &ast::QueryList, b: &ast::QueryList) -> bool {
    // Filter out QueryLines entries (whitespace) for comparison
    let a_queries: Vec<_> = a
        .queries
        .iter()
        .filter(|q| !matches!(q, ast::QueryDef::QueryLines { .. }))
        .collect();
    let b_queries: Vec<_> = b
        .queries
        .iter()
        .filter(|q| !matches!(q, ast::QueryDef::QueryLines { .. }))
        .collect();

    if a_queries.len() != b_queries.len() {
        return false;
    }

    for (qa, qb) in a_queries.iter().zip(b_queries.iter()) {
        if !query_def_equal_ignoring_locations(qa, qb) {
            return false;
        }
    }

    true
}

fn query_def_equal_ignoring_locations(a: &ast::QueryDef, b: &ast::QueryDef) -> bool {
    match (a, b) {
        (ast::QueryDef::Query(qa), ast::QueryDef::Query(qb)) => {
            query_equal_ignoring_locations(qa, qb)
        }
        (ast::QueryDef::QueryComment { text: ta }, ast::QueryDef::QueryComment { text: tb }) => {
            ta == tb
        }
        (ast::QueryDef::QueryLines { count: ca }, ast::QueryDef::QueryLines { count: cb }) => {
            ca == cb
        }
        _ => false,
    }
}

fn query_equal_ignoring_locations(a: &ast::Query, b: &ast::Query) -> bool {
    // Note: We ignore interface_hash and full_hash as they may differ
    // due to formatting changes affecting whitespace
    a.operation == b.operation
        && a.name == b.name
        && query_params_equal_ignoring_locations(&a.args, &b.args)
        && top_level_query_fields_equal_ignoring_locations(&a.fields, &b.fields)
}

fn query_params_equal_ignoring_locations(
    a: &Vec<ast::QueryParamDefinition>,
    b: &Vec<ast::QueryParamDefinition>,
) -> bool {
    if a.len() != b.len() {
        return false;
    }

    for (pa, pb) in a.iter().zip(b.iter()) {
        if pa.name != pb.name || pa.type_ != pb.type_ {
            return false;
        }
    }

    true
}

fn top_level_query_fields_equal_ignoring_locations(
    a: &Vec<ast::TopLevelQueryField>,
    b: &Vec<ast::TopLevelQueryField>,
) -> bool {
    if a.len() != b.len() {
        return false;
    }

    for (fa, fb) in a.iter().zip(b.iter()) {
        if !top_level_query_field_equal_ignoring_locations(fa, fb) {
            return false;
        }
    }

    true
}

fn top_level_query_field_equal_ignoring_locations(
    a: &ast::TopLevelQueryField,
    b: &ast::TopLevelQueryField,
) -> bool {
    match (a, b) {
        (ast::TopLevelQueryField::Field(fa), ast::TopLevelQueryField::Field(fb)) => {
            query_field_equal_ignoring_locations(fa, fb)
        }
        (
            ast::TopLevelQueryField::Lines { count: ca },
            ast::TopLevelQueryField::Lines { count: cb },
        ) => ca == cb,
        (
            ast::TopLevelQueryField::Comment { text: ta },
            ast::TopLevelQueryField::Comment { text: tb },
        ) => ta == tb,
        _ => false,
    }
}

fn query_field_equal_ignoring_locations(a: &ast::QueryField, b: &ast::QueryField) -> bool {
    a.name == b.name
        && a.alias == b.alias
        && match (&a.set, &b.set) {
            (None, None) => true,
            (Some(sa), Some(sb)) => query_value_equal_ignoring_locations(sa, sb),
            _ => false,
        }
        && a.directives == b.directives
        && arg_fields_equal_ignoring_locations(&a.fields, &b.fields)
}

fn arg_fields_equal_ignoring_locations(a: &Vec<ast::ArgField>, b: &Vec<ast::ArgField>) -> bool {
    // Filter out Lines entries (whitespace) for comparison
    let a_fields: Vec<_> = a
        .iter()
        .filter(|f| !matches!(f, ast::ArgField::Lines { .. }))
        .collect();
    let b_fields: Vec<_> = b
        .iter()
        .filter(|f| !matches!(f, ast::ArgField::Lines { .. }))
        .collect();

    // Separate args (limit, sort, where) from fields and comments
    let mut a_limits = Vec::new();
    let mut a_sorts = Vec::new();
    let mut a_wheres = Vec::new();
    let mut a_fields_list = Vec::new();
    let mut a_comments = Vec::new();

    let mut b_limits = Vec::new();
    let mut b_sorts = Vec::new();
    let mut b_wheres = Vec::new();
    let mut b_fields_list = Vec::new();
    let mut b_comments = Vec::new();

    for f in a_fields.iter() {
        match f {
            ast::ArgField::Arg(located_arg) => match &located_arg.arg {
                ast::Arg::Limit(_) => a_limits.push(f),
                ast::Arg::OrderBy(_, _) => a_sorts.push(f),
                ast::Arg::Where(_) => a_wheres.push(f),
            },
            ast::ArgField::Field(_) => a_fields_list.push(f),
            ast::ArgField::QueryComment { .. } => a_comments.push(f),
            _ => (),
        }
    }

    for f in b_fields.iter() {
        match f {
            ast::ArgField::Arg(located_arg) => match &located_arg.arg {
                ast::Arg::Limit(_) => b_limits.push(f),
                ast::Arg::OrderBy(_, _) => b_sorts.push(f),
                ast::Arg::Where(_) => b_wheres.push(f),
            },
            ast::ArgField::Field(_) => b_fields_list.push(f),
            ast::ArgField::QueryComment { .. } => b_comments.push(f),
            _ => (),
        }
    }

    // Compare limits (order doesn't matter, but there should only be one)
    if a_limits.len() != b_limits.len() {
        return false;
    }
    for limit_a in a_limits.iter() {
        let mut found = false;
        for limit_b in b_limits.iter() {
            if arg_field_equal_ignoring_locations(limit_a, limit_b) {
                found = true;
                break;
            }
        }
        if !found {
            return false;
        }
    }

    // Compare sorts (order doesn't matter)
    if a_sorts.len() != b_sorts.len() {
        return false;
    }
    for sort_a in a_sorts.iter() {
        let mut found = false;
        for sort_b in b_sorts.iter() {
            if arg_field_equal_ignoring_locations(sort_a, sort_b) {
                found = true;
                break;
            }
        }
        if !found {
            return false;
        }
    }

    // Compare wheres (order doesn't matter)
    if a_wheres.len() != b_wheres.len() {
        return false;
    }
    for where_a in a_wheres.iter() {
        let mut found = false;
        for where_b in b_wheres.iter() {
            if arg_field_equal_ignoring_locations(where_a, where_b) {
                found = true;
                break;
            }
        }
        if !found {
            return false;
        }
    }

    // Compare fields (order matters)
    if a_fields_list.len() != b_fields_list.len() {
        return false;
    }
    for (fa, fb) in a_fields_list.iter().zip(b_fields_list.iter()) {
        if !arg_field_equal_ignoring_locations(fa, fb) {
            return false;
        }
    }

    // Compare comments (order matters)
    if a_comments.len() != b_comments.len() {
        return false;
    }
    for (ca, cb) in a_comments.iter().zip(b_comments.iter()) {
        if !arg_field_equal_ignoring_locations(ca, cb) {
            return false;
        }
    }

    true
}

fn arg_field_equal_ignoring_locations(a: &ast::ArgField, b: &ast::ArgField) -> bool {
    match (a, b) {
        (ast::ArgField::Field(fa), ast::ArgField::Field(fb)) => {
            query_field_equal_ignoring_locations(fa, fb)
        }
        (ast::ArgField::Arg(la), ast::ArgField::Arg(lb)) => {
            arg_equal_ignoring_locations(&la.arg, &lb.arg)
        }
        (ast::ArgField::Lines { count: ca }, ast::ArgField::Lines { count: cb }) => ca == cb,
        (ast::ArgField::QueryComment { text: ta }, ast::ArgField::QueryComment { text: tb }) => {
            ta == tb
        }
        _ => false,
    }
}

fn arg_equal_ignoring_locations(a: &ast::Arg, b: &ast::Arg) -> bool {
    match (a, b) {
        (ast::Arg::Limit(va), ast::Arg::Limit(vb)) => query_value_equal_ignoring_locations(va, vb),
        (ast::Arg::OrderBy(da, sa), ast::Arg::OrderBy(db, sb)) => match (da, db) {
            (ast::Direction::Asc, ast::Direction::Asc)
            | (ast::Direction::Desc, ast::Direction::Desc) => sa == sb,
            _ => false,
        },
        (ast::Arg::Where(wa), ast::Arg::Where(wb)) => where_arg_equal_ignoring_locations(wa, wb),
        _ => false,
    }
}

/// Round trip test helper for schemas
fn round_trip_schema(source: &str) {
    // Parse original
    let mut schema1 = ast::Schema::default();
    let parse_result1 = parser::run("schema.pyre", source, &mut schema1);
    assert!(
        parse_result1.is_ok(),
        "First parse should succeed. Error: {:?}",
        parse_result1.err()
    );

    // Format
    format::schema(&mut schema1);
    let formatted = generate::to_string::schema_to_string(&schema1.namespace, &schema1);

    // Parse formatted
    let mut schema2 = ast::Schema::default();
    let parse_result2 = parser::run("schema.pyre", &formatted, &mut schema2);
    assert!(
        parse_result2.is_ok(),
        "Second parse should succeed. Error: {:?}\nFormatted output:\n{}",
        parse_result2.err(),
        formatted
    );

    // Format again
    format::schema(&mut schema2);

    // Compare ASTs (ignoring locations)
    assert!(
        schemas_equal_ignoring_locations(&schema1, &schema2),
        "ASTs should match after round trip. Original:\n{}\n\nFormatted:\n{}",
        source,
        formatted
    );
}

fn format_schema_string(source: &str) -> String {
    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", source, &mut schema).expect("schema should parse");
    format::schema(&mut schema);
    generate::to_string::schema_to_string(&schema.namespace, &schema)
}

#[test]
fn test_top_level_comment_stays_attached_to_declaration() {
    let formatted = format_schema_string(
        r#"// I want to be able to leave a comment here
type Annotation
  = ShapeAnnotation { shape Shape }
  | LineAnnotation { line Line }
"#,
    );

    assert!(
        formatted.starts_with("// I want to be able to leave a comment here\ntype Annotation\n"),
        "comment should not be separated from its declaration:\n{}",
        formatted
    );
}

#[test]
fn test_top_level_comment_allows_one_blank_before_declaration() {
    let formatted = format_schema_string(
        r#"// This comment is intentionally separated


record User {
    id Int @id
}
"#,
    );

    assert!(
        formatted.starts_with("// This comment is intentionally separated\n\nrecord User {\n"),
        "comment should allow at most one blank line before declaration:\n{}",
        formatted
    );
}

#[test]
fn test_declaration_always_has_blank_line_after_it() {
    let formatted = format_schema_string(
        r#"record User {
    id Int @id
}
// Comment about posts
record Post {
    id Int @id
}
"#,
    );

    assert!(
        formatted.contains("}\n\n// Comment about posts\nrecord Post"),
        "declaration should be separated from following comment:\n{}",
        formatted
    );
}

#[test]
fn test_timestamps_grouped_with_table_directives() {
    let formatted = format_schema_string(
        r#"record Note {
    id Int @id
    body String
    @index(body)
    @public
    @timestamps
    @watch
    @unique(body)
    @tablename("notes")
}
"#,
    );

    assert_eq!(
        formatted,
        "record Note {\n    @tablename(\"notes\")\n    @watch\n    @public\n    @timestamps\n    @unique(body asc)\n    @index(body asc)\n\n    id   Int    @id\n    body String\n}\n"
    );
}

/// Round trip test helper for queries
fn round_trip_query(source: &str, database: &ast::Database) {
    // Parse original
    let query_list1 = parser::parse_query("query.pyre", source);
    assert!(
        query_list1.is_ok(),
        "First parse should succeed. Error: {:?}",
        query_list1.err()
    );
    let mut query_list1 = query_list1.unwrap();

    // Format
    format::query_list(database, &mut query_list1);
    let formatted = generate::to_string::query(&query_list1);

    // Parse formatted
    let query_list2 = parser::parse_query("query.pyre", &formatted);
    assert!(
        query_list2.is_ok(),
        "Second parse should succeed. Error: {:?}\nFormatted output:\n{}",
        query_list2.err(),
        formatted
    );
    let mut query_list2 = query_list2.unwrap();

    // Format again
    format::query_list(database, &mut query_list2);

    // Compare ASTs (ignoring locations)
    assert!(
        query_lists_equal_ignoring_locations(&query_list1, &query_list2),
        "ASTs should match after round trip. Original:\n{}\n\nFormatted:\n{}",
        source,
        formatted
    );
}

// ============================================================================
// Schema Round Trip Tests
// ============================================================================

#[test]
fn test_schema_round_trip_comprehensive() {
    let schema_source = r#"
// Comment at the top
record User {
    id Int @id
    name String
    email String @unique
    age Int?
    createdAt DateTime @default(now)
    updatedAt DateTime @default(now)
    @tablename("users")
    @allow(query) { id == Session.userId }
    @allow(insert, update) { id == Session.userId }
}

record Post {
    id Int @id
    title String
    content String?
    authorId Int
    author @link(authorId, User.id)
    published Bool @default(False)
    createdAt DateTime @default(now)
    @allow(*) { authorId == Session.userId }
}

record Comment {
    id Int @id
    postId Int
    userId Int
    content String
    post @link(postId, Post.id)
    user @link(userId, User.id)
    createdAt DateTime @default(now)
    @allow(query) { userId == Session.userId }
    @allow(insert, update) { userId == Session.userId }
}

type Status
    = Active
    | Inactive
    | Pending {
        reason String
    }

session {
    userId Int
    role String
}
    "#;

    round_trip_schema(schema_source);
}

#[test]
fn test_schema_round_trip_directives() {
    let schema_source = r#"
record Test {
    id Int @id
    uniqueField String @unique
    indexedField String @index
    defaultField String @default("test")
    defaultNow DateTime @default(now)
    nullableField String?
    @tablename("test_table")
    @public
}
    "#;

    round_trip_schema(schema_source);
}

#[test]
fn test_schema_round_trip_table_level_index_directives() {
    let schema_source = r#"
record Membership {
    id        Int @id
    orgId     Int
    userId    Int
    createdAt DateTime
    deletedAt DateTime?

    @unique(orgId, userId)
    @index(orgId asc, createdAt desc) where {
        deletedAt = null
    }
    @public
}
    "#;

    round_trip_schema(schema_source);
}

#[test]
fn test_schema_round_trip_permissions() {
    let schema_source = r#"
record Post {
    id Int @id
    title String
    authorId Int
    @allow(query) { published == True }
    @allow(insert, update) { authorId == Session.userId && status == "draft" }
    @allow(delete) { authorId == Session.userId || Session.role == "admin" }
}
    "#;

    round_trip_schema(schema_source);
}

#[test]
fn test_allow_multiterm_or_formats_multiline() {
    let schema_source = r#"
record Game {
    @allow(*) { createdByUserId == Session.userId || Session.isAdmin == True }
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).unwrap();
    format::schema(&mut schema);
    let formatted = generate::to_string::schema_to_string(&schema.namespace, &schema);

    assert!(
        formatted.contains("@allow(*) {\n        createdByUserId == Session.userId\n        || Session.isAdmin == True\n"),
        "Expected multi-term @allow OR to format as multi-line. Got:\n{}",
        formatted
    );
}

#[test]
fn test_allow_single_term_stays_single_line() {
    let schema_source = r#"
record Game {
    @allow(*) { createdByUserId == Session.userId }
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).unwrap();
    format::schema(&mut schema);
    let formatted = generate::to_string::schema_to_string(&schema.namespace, &schema);

    assert!(
        formatted.contains("@allow(*) { createdByUserId == Session.userId"),
        "Expected single-term @allow to stay single-line. Got:\n{}",
        formatted
    );
    assert!(
        !formatted.contains("@allow(*) {\n"),
        "Single-term @allow should not be multiline. Got:\n{}",
        formatted
    );
}

#[test]
fn test_schema_round_trip_links() {
    let schema_source = r#"
record User {
    id Int @id
    name String
}

record Post {
    id Int @id
    authorId Int
    author @link(authorId, User.id)
}

record Comment {
    id Int @id
    postId Int
    userId Int
    post @link(postId, Post.id)
    user @link(userId, User.id)
}
    "#;

    round_trip_schema(schema_source);
}

#[test]
fn test_format_preserves_explicit_inverse_link_name() {
    let schema_source = r#"
record RulebookRules {
    id Int @id
    versions @link(RulebookVersionRules.rulebookRulesId)
}

record RulebookVersionRules {
    id Int @id
    rulebookRulesId RulebookRules.id
    rulebookRules @link(rulebookRulesId, RulebookRules.id)
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).unwrap();
    format::schema(&mut schema);
    let formatted = generate::to_string::schema_to_string(&schema.namespace, &schema);

    assert!(
        formatted.contains("versions @link(RulebookVersionRules.rulebookRulesId)"),
        "Expected explicit inverse link name to be preserved. Got:\n{}",
        formatted
    );
    assert!(
        !formatted.contains("rulebookVersionRules @link(RulebookVersionRules.rulebookRulesId)"),
        "Expected generated link name not to replace explicit one. Got:\n{}",
        formatted
    );
}

#[test]
fn test_schema_round_trip_tagged_types() {
    let schema_source = r#"
type SimpleTagged
   = Option1
   | Option2
   | Option3

type TaggedWithFields
   = Active
   | Inactive
   | Pending {
        reason String
        createdAt DateTime
    }

record Test {
    id Int @id
    status TaggedWithFields
}
    "#;

    round_trip_schema(schema_source);
}

// ============================================================================
// Query Round Trip Tests
// ============================================================================

fn create_test_database() -> ast::Database {
    let schema_source = r#"
record User {
    id Int @id
    name String
    email String
    age Int?
}

record Post {
    id Int @id
    title String
    content String?
    authorId Int
    author @link(authorId, User.id)
    published Bool
}

session {
    userId Int
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).unwrap();
    ast::Database {
        schemas: vec![schema],
    }
}

fn create_id_type_database() -> ast::Database {
    let schema_source = r#"
record Task {
    @public
    id Id.Int @id
    description String
}
    "#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).unwrap();
    ast::Database {
        schemas: vec![schema],
    }
}

#[test]
fn test_query_round_trip_simple_select() {
    let database = create_test_database();
    let query_source = r#"
query GetUsers {
    user {
        id
        name
        email
    }
}
    "#;

    round_trip_query(query_source, &database);
}

#[test]
fn test_query_round_trip_with_params() {
    let database = create_test_database();
    let query_source = r#"
 query GetUser($id: Int) {
    user {
        @where { id == $id }
        id
        name
        email
    }
}
    "#;

    round_trip_query(query_source, &database);
}

#[test]
fn test_query_format_infers_id_type_param() {
    let database = create_id_type_database();
    let query_source = r#"
query GetTask($id) {
    task {
        @where { id == $id }
        id
        description
    }
}
    "#;

    let mut query_list = parser::parse_query("query.pyre", query_source).unwrap();
    format::query_list(&database, &mut query_list);
    let formatted = generate::to_string::query(&query_list);

    assert!(
        formatted.contains("$id: Task.id"),
        "Formatted output should include Task.id type. Got:\n{}",
        formatted
    );
}

#[test]
fn test_query_round_trip_nested() {
    let database = create_test_database();
    let query_source = r#"
query GetPostsWithAuthor {
    post {
        id
        title
        author {
            id
            name
        }
    }
}
    "#;

    round_trip_query(query_source, &database);
}

#[test]
fn test_query_round_trip_with_where() {
    let database = create_test_database();
    let query_source = r#"
query GetPublishedPosts {
    post {
        @where { published == True }
        id
        title
    }
}
    "#;

    round_trip_query(query_source, &database);
}

#[test]
fn test_query_round_trip_with_limit() {
    let database = create_test_database();
    let query_source = r#"
query GetUsers {
    user {
        @limit(10)
        id
        name
    }
}
    "#;

    round_trip_query(query_source, &database);
}

#[test]
fn test_query_round_trip_with_order_by() {
    let database = create_test_database();
    let query_source = r#"
query GetUsers {
    user {
        @sort(name, Asc)
        id
        name
    }
}
    "#;

    round_trip_query(query_source, &database);
}

#[test]
fn test_query_round_trip_insert() {
    let database = create_test_database();
    let query_source = r#"
insert CreateUser($name: String, $email: String) {
    user {
        name = $name
        email = $email
        id
        name
        email
    }
}
    "#;

    round_trip_query(query_source, &database);
}

#[test]
fn test_query_round_trip_update() {
    let database = create_test_database();
    let query_source = r#"
update UpdateUser($id: Int, $name: String) {
    user {
        @where { id == $id }
        name = $name
        id
        name
    }
}
    "#;

    round_trip_query(query_source, &database);
}

#[test]
fn test_query_round_trip_delete() {
    let database = create_test_database();
    let query_source = r#"
delete DeleteUser($id: Int) {
    user {
        @where { id == $id }
        id
    }
}
    "#;

    round_trip_query(query_source, &database);
}

#[test]
fn test_query_round_trip_complex_where() {
    let database = create_test_database();
    let query_source = r#"
query GetUsers {
    user {
        @where { id == 1 && name == "test" || email == "test@example.com" }
        id
        name
    }
}
    "#;

    round_trip_query(query_source, &database);
}

#[test]
fn test_query_round_trip_with_session() {
    let database = create_test_database();
    let query_source = r#"
query GetCurrentUser {
    user {
        @where { id == Session.userId }
        id
        name
        email
    }
}
    "#;

    round_trip_query(query_source, &database);
}

#[test]
fn test_query_round_trip_multiple_queries() {
    let database = create_test_database();
    let query_source = r#"
query GetUsers {
    user {
        id
        name
    }
}

query GetPosts {
    post {
        id
        title
    }
}
    "#;

    round_trip_query(query_source, &database);
}

#[test]
fn test_query_round_trip_with_comments() {
    let database = create_test_database();
    let query_source = r#"
// Get all users
query GetUsers {
    user {
        id
        name
    }
}
    "#;

    round_trip_query(query_source, &database);
}

#[test]
fn test_schema_single_pass_alignment() {
    // Test that formatting aligns both types and directives in a single pass
    let schema_source = r#"
record Task {
    id String @id
    description String
    status TaskStatus
    createdAt DateTime @default(now)
    updatedAt DateTime @default(now)
    maxIterations Int @default(50)
    currentIteration Int @default(0)
}
    "#;

    // Parse original
    let mut schema1 = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema1).unwrap();

    // Format once
    format::schema(&mut schema1);
    let formatted_once = generate::to_string::schema_to_string(&schema1.namespace, &schema1);

    // Parse and format again
    let mut schema2 = ast::Schema::default();
    parser::run("schema.pyre", &formatted_once, &mut schema2).unwrap();
    format::schema(&mut schema2);
    let formatted_twice = generate::to_string::schema_to_string(&schema2.namespace, &schema2);

    // The output should be identical after one format vs two formats
    assert_eq!(
        formatted_once, formatted_twice,
        "Formatting should be idempotent. First format:\n{}\n\nSecond format:\n{}",
        formatted_once, formatted_twice
    );
}
