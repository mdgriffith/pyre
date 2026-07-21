use crate::ast;

use crate::generate::sql::to_sql;
use crate::typecheck;
use std::collections::HashSet;

/*

So, there's a potentially spicy decision to have SQLite handle composing the JSON for us.

This dramatically reduces the generated client code.


// Query

    query Games {
        game {
            @where { ownerId = Session.userId }
            id
            name
            myPlayersAlias: players {
                id
                aliasedName: name
                points
                user {
                    id
                    name
                    posts {
                        title
                        content
                    }
                }
            }
        }
    }


// Example SQL
//

    WITH
    GamesFilter AS (
        SELECT id, name
        FROM games
        WHERE owner_id = :userId
    ),
    PlayersForGames AS (
        SELECT
            game_id,
            JSON_GROUP_ARRAY(
                JSON_OBJECT(
                    'id', id,
                    'aliasedName', name,
                    'points', points
                )
            ) AS players_json
        FROM players
        WHERE game_id IN (SELECT id FROM GamesFilter)
        GROUP BY game_id
    )

    SELECT
        JSON_OBJECT(
            'game', JSON_GROUP_ARRAY(
                JSON_OBJECT(
                    'id', g.id,
                    'name', g.name,
                    'players', COALESCE(p.players_json, '[]')
                )
            )
        ) AS result
    FROM GamesFilter g
    LEFT JOIN PlayersForGames p ON g.id = p.game_id;






*/

pub fn select_to_string(
    context: &typecheck::Context,
    query: &ast::Query,
    query_info: &typecheck::QueryInfo,
    table: &typecheck::Table,
    query_table_field: &ast::QueryField,
) -> Vec<to_sql::Prepared> {
    let all_query_fields = ast::collect_query_fields(&query_table_field.fields);

    let mut last_link_index = 0;
    for (i, query_field) in all_query_fields.iter().enumerate() {
        if query_field.name == "*" {
            continue;
        }
        if let Some(table_field) = table
            .record
            .fields
            .iter()
            .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
        {
            match table_field {
                ast::Field::FieldDirective(ast::FieldDirective::Link(_)) => {
                    last_link_index = i;
                }
                _ => (),
            }
        }
    }

    let initial_indent = if last_link_index == 0 { 0 } else { 4 };
    let mut statements = to_sql::format_attach(query_info);

    let mut result = String::new();

    let initial_selection = initial_select(
        initial_indent,
        context,
        query_info,
        table,
        query_table_field,
    );
    let query_path = ast::get_aliased_name(query_table_field);
    let parent_table_alias = &get_temp_table_name_for_path(&query_path);

    let mut rendered_initial = false;
    let mut has_links = false;

    for query_field in all_query_fields.iter() {
        if query_field.name == "*" {
            continue;
        }
        match table
            .record
            .fields
            .iter()
            .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
        {
            Some(table_field) => {
                match table_field {
                    ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
                        has_links = true;
                        // We are inserting a link, so we need to do a nested insert
                        match typecheck::get_linked_table(context, &link) {
                            Some(linked_table) => {
                                if !rendered_initial {
                                    result.push_str("with ");
                                    result.push_str(parent_table_alias);
                                    result.push_str(" as (\n");
                                    result.push_str(&initial_selection);
                                    result.push_str("\n)");
                                    rendered_initial = true;
                                }

                                let linked_path = format!(
                                    "{}__{}",
                                    query_path,
                                    ast::get_aliased_name(query_field)
                                );
                                let temp_table_alias = &get_temp_table_name_for_path(&linked_path);

                                result.push_str(",");
                                result.push_str(" ");
                                result.push_str(temp_table_alias);
                                result.push_str(" as (");

                                select_linked(
                                    4,
                                    context,
                                    query,
                                    query_info,
                                    parent_table_alias,
                                    linked_table,
                                    query_field,
                                    &linked_path,
                                    link,
                                    &mut result,
                                );

                                result.push_str(")");
                            }
                            None => {
                                eprintln!("Warning: Linked table '{}' for link '{}' was not found in typecheck context. This may happen if the linked table is in a different schema. Skipping link generation.", link.foreign.table, link.link_name);
                            }
                        }
                    }
                    _ => (),
                }
            }
            None => {
                eprintln!("Warning: Field '{}' referenced in query was not found in table '{}'. This should not happen after successful typechecking. Skipping field generation.", query_field.name, table.record.name);
            }
        }
    }

    // If there are no links, we still need to create the initial CTE
    // because final_select_formatted_as_json always references it
    if !has_links && !rendered_initial {
        result.push_str("with ");
        result.push_str(parent_table_alias);
        result.push_str(" as (\n");
        result.push_str(&initial_selection);
        result.push_str("\n)\n");
    }

    // The final selection
    final_select_formatted_as_json(
        0,
        context,
        table,
        query_table_field,
        &query_path,
        &mut result,
    );

    statements.push(to_sql::include(result));
    statements
}

pub fn get_temp_table_name(query_field: &ast::QueryField) -> String {
    get_temp_table_name_for_path(&ast::get_aliased_name(query_field))
}

pub fn get_json_temp_table_name(query_field: &ast::QueryField) -> String {
    get_json_temp_table_name_for_path(&ast::get_aliased_name(query_field))
}

fn get_temp_table_name_for_path(path: &str) -> String {
    format!("temp_selected_{}", path)
}

fn get_json_temp_table_name_for_path(path: &str) -> String {
    format!("json__{}", path)
}

pub fn initial_select(
    indent: usize,
    context: &typecheck::Context,
    query_info: &typecheck::QueryInfo,
    table: &typecheck::Table,
    query_field: &ast::QueryField,
) -> String {
    let mut result = String::new();
    let indent_str = " ".repeat(indent);
    result.push_str(&indent_str);
    result.push_str("select ");

    let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
    let fieldnames = &to_fieldnames(context, table, &query_field.fields);
    result.push_str(&fieldnames.join(", "));
    result.push_str("\n");
    result.push_str(&indent_str);
    result.push_str("from ");
    result.push_str(&table_name);
    result.push_str("\n");

    to_sql::render_where(
        context,
        table,
        query_info,
        query_field,
        &ast::QueryOperation::Query,
        &mut result,
    );

    // Order by
    to_sql::render_order_by(Some(table), Some(query_info), query_field, &mut result);

    // LIMIT
    to_sql::render_limit(query_field, &mut result);

    result
}

fn select_linked(
    indent: usize,
    context: &typecheck::Context,
    query: &ast::Query,
    query_info: &typecheck::QueryInfo,
    parent_table_name: &String,
    table: &typecheck::Table,
    query_field: &ast::QueryField,
    query_path: &str,
    link: &ast::LinkDetails,

    //
    sql: &mut String,
) {
    /* This funciton can produce a few different sql forms


    1. If we're selecting through any links

        -- initial selection of data
        selected_myPlayersAlias as (
            select gameId, id, name, points
            from players
            where gameId in (select id from selected_game)
        )

        --> recursively select_linked data

        -- Formatted as JSON
        json_myPlayersAlias AS (
            select
                game_id,
                json_group_array(
                    json_object(
                        'id', selected_myPlayersAlias.player_id,
                        'aliasedName', selected_myPlayersAlias.player_name,
                        'points', selected_myPlayersAlias.player_points,
                        'user', COALESCE(uf.user_json, '{}')
                    )
                ) AS players_json
            from selected_myPlayersAlias selected_myPlayersAlias
            // Join children data
            left join UsersForPlayers uf ON selected_myPlayersAlias.player_id = uf.player_id
            group by game_id
        )


    2. If there are *NO* links, we select the full selection as json

        selected_myPlayersAlias as (
            select
                gameId,
                JSON_OBJECT(
                    'id', id,
                    'name', name,
                    'points', points
                )
            from players
            where gameId in (select id from selected_game)
            group
        )

        There is no need to `group`` or `json_group_array` in this case.


     */

    if !selects_for_link(query_field, table) {
        select_single_json(
            indent,
            context,
            query_info,
            parent_table_name,
            table,
            query_field,
            link,
            sql,
        );
        return;
    }

    let indent_str = " ".repeat(indent);

    let mut field_names: Vec<String> = Vec::new();
    let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
    let new_fieldnames = &to_fieldnames(context, table, &query_field.fields);
    field_names.push(link.foreign.fields.clone().join(", "));
    field_names.append(&mut new_fieldnames.clone());
    // field_names.push

    // initial selection
    // Use table alias for JOIN
    let table_alias = "t";
    sql.push_str(&format!(
        "\n{}select {}\n{}from {} {}\n",
        indent_str,
        field_names
            .iter()
            .map(|f| format!("{}.{}", table_alias, f))
            .collect::<Vec<_>>()
            .join(", "),
        indent_str,
        table_name,
        table_alias,
    ));

    let all_query_fields = ast::collect_query_fields(&query_field.fields);

    let mut full_local_id = String::new();
    for local_id in &link.local_ids {
        full_local_id.push_str(local_id);
    }

    // This is the link.foreign_id, which is the id on this table
    let mut full_foreign_id = String::new();
    for foreign_id in &link.foreign.fields {
        full_foreign_id.push_str(foreign_id);
    }

    // Check if render_where will add a WHERE clause
    let mut where_check = String::new();
    to_sql::render_where_with_table_ref(
        context,
        table,
        query_info,
        query_field,
        &ast::QueryOperation::Query,
        Some(table_alias),
        &mut where_check,
    );
    let has_where = where_check.contains("where");

    to_sql::render_where_with_table_ref(
        context,
        table,
        query_info,
        query_field,
        &ast::QueryOperation::Query,
        Some(table_alias),
        sql,
    );

    // Use WHERE ... IN (SELECT ...) - JOIN optimization causes memory issues in some cases
    if has_where {
        sql.push_str(&format!(
            "{}and {}.{} in (select {} from {})\n",
            indent_str, table_alias, full_foreign_id, full_local_id, parent_table_name
        ));
    } else {
        sql.push_str(&format!(
            "{}where {}.{} in (select {} from {})\n",
            indent_str, table_alias, full_foreign_id, full_local_id, parent_table_name
        ));
    }
    // result.push_str(&format!("{}from {}", indent_str, parent_table_name));

    // result.push_str(&format!("{}group by {}", indent_str, full_foreign_id));

    // Recursively define children
    let parent_temp_table = &get_temp_table_name_for_path(query_path);

    for query_field in all_query_fields {
        if let Some(table_field) = table
            .record
            .fields
            .iter()
            .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
        {
            let linked_path = format!("{}__{}", query_path, ast::get_aliased_name(query_field));
            let temp_table_name = &get_temp_table_name_for_path(&linked_path);

            match table_field {
                ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
                    // We are inserting a link, so we need to do a nested insert
                    if let Some(linked_table) = typecheck::get_linked_table(context, &link) {
                        sql.push_str(&format!("), {} as (", temp_table_name));
                        select_linked(
                            indent,
                            context,
                            query,
                            query_info,
                            parent_temp_table,
                            linked_table,
                            query_field,
                            &linked_path,
                            &link,
                            sql,
                        );
                    }
                }
                _ => (),
            }
        }
    }

    let json_table_name = &get_json_temp_table_name_for_path(query_path);

    sql.push_str(&format!("), {} as (", json_table_name));
    // Format as JSON
    select_formatted_as_json(indent, context, table, query_field, query_path, link, sql);
}

fn selects_for_link(query: &ast::QueryField, table: &typecheck::Table) -> bool {
    for query_field in &query.fields {
        match query_field {
            ast::ArgField::Field(qf) => {
                let link_present = table
                    .record
                    .fields
                    .iter()
                    .any(|f| ast::has_link_named(&f, &qf.name));

                if link_present {
                    return true;
                }
            }
            _ => continue,
        }
    }
    return false;
}

/*

A simple as possible json selection

     selected_myPlayersAlias as (
        select
            gameId,
            JSON_OBJECT(
                'id', id,
                'name', name,
                'points', points
            )
        from players
        where gameId in (select id from selected_game)
        group
    )



*/
fn select_single_json(
    indent: usize,
    context: &typecheck::Context,
    query_info: &typecheck::QueryInfo,

    parent_table_name: &String,
    table: &typecheck::Table,
    query_table_field: &ast::QueryField,
    link: &ast::LinkDetails,

    //
    sql: &mut String,
) {
    let indent_str = " ".repeat(indent);
    let aggregate_to_array = !link_returns_singular_result(context, &table.record, link);

    // This is the link.foreign_id, which is the id on this table
    let mut full_foreign_id = String::new();
    for foreign_id in &link.foreign.fields {
        full_foreign_id.push_str(foreign_id);
    }

    let query_aliased_as = &ast::get_aliased_name(&query_table_field);
    let table_alias = "t";

    // Compose main json payload
    let mut json_object = String::new();
    let mut first_field = true;
    let mut explicit_columns = HashSet::new();
    for field in &query_table_field.fields {
        if let ast::ArgField::Field(query_field) = field {
            if query_field.name == "*" {
                continue;
            }
            if let Some(ast::Field::Column(column)) = table
                .record
                .fields
                .iter()
                .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
            {
                explicit_columns.insert(column.name.clone());
            }
        }
    }

    let mut rendered_columns: HashSet<String> = HashSet::new();

    for field in &query_table_field.fields {
        match field {
            ast::ArgField::Field(query_field) => {
                if query_field.name == "*" {
                    for table_field in &table.record.fields {
                        if let ast::Field::Column(column) = table_field {
                            if explicit_columns.contains(&column.name) {
                                continue;
                            }

                            if rendered_columns.insert(column.name.clone()) {
                                if !first_field {
                                    json_object.push_str(",\n");
                                }

                                json_object
                                    .push_str(&format!("{}    '{}', ", indent_str, column.name));
                                select_type(
                                    indent + 4,
                                    context,
                                    column,
                                    table_alias,
                                    &column.name,
                                    &mut json_object,
                                );

                                first_field = false;
                            }
                        }
                    }

                    continue;
                }

                if let Some(ast::Field::Column(column)) = table
                    .record
                    .fields
                    .iter()
                    .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
                {
                    let aliased_field_name = ast::get_aliased_name(query_field);

                    if rendered_columns.insert(aliased_field_name.clone()) {
                        if !first_field {
                            json_object.push_str(",\n");
                        }

                        json_object
                            .push_str(&format!("{}    '{}', ", indent_str, aliased_field_name));
                        select_type(
                            indent + 4,
                            context,
                            column,
                            table_alias,
                            &query_field.name,
                            &mut json_object,
                        );

                        first_field = false;
                    }
                }
            }
            ast::ArgField::Arg(_)
            | ast::ArgField::Lines { .. }
            | ast::ArgField::QueryComment { .. } => continue,
        }
    }

    // initial selection
    #[rustfmt::skip]
    let array_agg_start = if aggregate_to_array  { "jsonb_group_array(" } else { "" };
    #[rustfmt::skip]
    let array_agg_end = if aggregate_to_array { ")" } else { "" };

    let table_name = ast::get_tablename(&table.record.name, &table.record.fields);

    sql.push_str(&format!(
        "\n{}select\n  {}{}.{},\n{}  {}jsonb_object(\n{}\n{}  ){} as {}\n{}from {} {}\n",
        indent_str,
        indent_str,
        table_alias,
        full_foreign_id,
        indent_str,
        array_agg_start,
        json_object,
        indent_str,
        array_agg_end,
        query_aliased_as,
        indent_str,
        table_name,
        table_alias,
    ));

    let mut full_local_id = String::new();
    for local_id in &link.local_ids {
        full_local_id.push_str(local_id);
    }

    let mut where_sql = String::new();
    to_sql::render_where_with_table_ref(
        context,
        table,
        query_info,
        query_table_field,
        &ast::QueryOperation::Query,
        Some(table_alias),
        &mut where_sql,
    );
    sql.push_str(&where_sql);

    // Use WHERE ... IN (SELECT ...) - JOIN optimization causes memory issues in some cases.
    let relationship_keyword = if where_sql.is_empty() { "where" } else { "and" };
    sql.push_str(&format!(
        "{}{} {}.{} in (select {} from {})\n",
        indent_str,
        relationship_keyword,
        table_alias,
        full_foreign_id,
        full_local_id,
        parent_table_name
    ));

    if aggregate_to_array {
        sql.push_str(&format!(
            "{}group by {}.{}\n",
            indent_str, table_alias, full_foreign_id
        ));
        // Add ORDER BY after GROUP BY to ensure deterministic ordering
        sql.push_str(&format!(
            "{}order by {}.{}\n",
            indent_str, table_alias, full_foreign_id
        ));
    }
}

/*

We're formatting a grouped table

     selected_myPlayersAlias as (
        select
            gameId,
            JSON_OBJECT(
                'id', id,
                'name', name,
                'points', points
            )
        from players
        where gameId in (select id from selected_game)
        group
    )

    json_myPlayersAlias AS (
        SELECT
            game_id,
            JSON_GROUP_ARRAY(
                JSON_OBJECT(
                    'id', pf.player_id,
                    'aliasedName', pf.player_name,
                    'points', pf.player_points,
                    'user', COALESCE(uf.user_json, '{}')
                )
            ) AS players_json
        FROM PlayersForGames pf
        LEFT JOIN UsersForPlayers uf ON pf.player_id = uf.player_id
        GROUP BY game_id
)



*/
fn select_formatted_as_json(
    indent: usize,
    context: &typecheck::Context,

    table: &typecheck::Table,
    query_table_field: &ast::QueryField,
    query_path: &str,
    link: &ast::LinkDetails,

    //
    sql: &mut String,
) {
    let indent_str = " ".repeat(indent);
    let aggregate_to_array = !link_returns_singular_result(context, &table.record, link);

    let aliased_name = ast::get_aliased_name(query_table_field);
    let base_table_name = get_temp_table_name_for_path(query_path);

    // This is the link.foreign_id, which is the id on this table
    let mut full_foreign_id = String::new();
    for foreign_id in &link.foreign.fields {
        full_foreign_id.push_str(foreign_id);
    }

    // initial selection
    let array_agg_start = if aggregate_to_array {
        "jsonb_group_array("
    } else {
        ""
    };
    sql.push_str(&format!(
        "\n{}select\n  {}{}.{},\n{}  {}jsonb_object(\n",
        indent_str, indent_str, base_table_name, full_foreign_id, indent_str, array_agg_start
    ));

    // Compose main json payload
    let mut first_field = true;
    let mut explicit_columns = HashSet::new();
    for field in &query_table_field.fields {
        if let ast::ArgField::Field(query_field) = field {
            if query_field.name == "*" {
                continue;
            }
            if let Some(ast::Field::Column(column)) = table
                .record
                .fields
                .iter()
                .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
            {
                explicit_columns.insert(column.name.clone());
            }
        }
    }
    let mut rendered_columns: HashSet<String> = HashSet::new();
    for field in &query_table_field.fields {
        match field {
            ast::ArgField::Field(query_field) => {
                if query_field.name == "*" {
                    for table_field in &table.record.fields {
                        if let ast::Field::Column(column) = table_field {
                            if explicit_columns.contains(&column.name) {
                                continue;
                            }
                            if rendered_columns.insert(column.name.clone()) {
                                render_json_column(
                                    indent,
                                    &indent_str,
                                    context,
                                    &base_table_name,
                                    &column.name,
                                    &column.name,
                                    column,
                                    &mut first_field,
                                    sql,
                                );
                            }
                        }
                    }
                    continue;
                }
                if let Some(table_field) = table
                    .record
                    .fields
                    .iter()
                    .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
                {
                    let aliased_field_name = ast::get_aliased_name(query_field);

                    match table_field {
                        ast::Field::Column(table_column) => {
                            if rendered_columns.insert(aliased_field_name.clone()) {
                                render_json_column(
                                    indent,
                                    &indent_str,
                                    context,
                                    &base_table_name,
                                    &aliased_field_name,
                                    &query_field.name,
                                    table_column,
                                    &mut first_field,
                                    sql,
                                );
                            }
                        }
                        ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
                            if !first_field {
                                sql.push_str(",\n");
                            }
                            if link_returns_singular_result(context, &table.record, link) {
                                // singular result, no need to coalesce

                                sql.push_str(&format!(
                                    "{}    '{}', temp__{}.{}",
                                    &indent_str,
                                    aliased_field_name,
                                    query_field.name,
                                    aliased_field_name,
                                ));
                            } else {
                                // Coalesce as an empty array
                                // Use jsonb() to ensure type compatibility with jsonb_group_array result
                                sql.push_str(&format!(
                                    "{}    '{}', coalesce(temp__{}.{}, jsonb('[]'))",
                                    &indent_str,
                                    aliased_field_name,
                                    query_field.name,
                                    aliased_field_name,
                                ));
                            }
                            first_field = false;
                        }
                        _ => continue,
                    }
                }
            }
            ast::ArgField::Arg(_)
            | ast::ArgField::Lines { .. }
            | ast::ArgField::QueryComment { .. } => continue,
        }
    }

    let close_expr = if aggregate_to_array { "))" } else { ")" };
    sql.push_str(&format!(
        "\n{}  {} as {}\n",
        indent_str, close_expr, aliased_name
    ));

    // FROM
    sql.push_str(&format!("{}from {}\n", indent_str, base_table_name));

    // Join every link
    for field in &query_table_field.fields {
        match field {
            ast::ArgField::Field(query_field) => {
                if let Some(table_field) = table
                    .record
                    .fields
                    .iter()
                    .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
                {
                    match table_field {
                        ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
                            if let Some(linked_table) = typecheck::get_linked_table(context, &link)
                            {
                                let linked_path = format!(
                                    "{}__{}",
                                    query_path,
                                    ast::get_aliased_name(query_field)
                                );
                                let query_temp_table =
                                    to_temp_table_alias(&linked_path, query_field, linked_table);
                                let local_temp_alias = format!("temp__{}", link.link_name);

                                sql.push_str(&format!(
                                    "{}  left join {} {} on {}.{} = {}.{}\n",
                                    indent_str,
                                    query_temp_table,
                                    local_temp_alias,
                                    local_temp_alias,
                                    link.foreign.fields.join(""),
                                    base_table_name,
                                    link.local_ids.join(""),
                                ));
                            }
                        }
                        _ => continue,
                    }
                }
            }
            ast::ArgField::Arg(_)
            | ast::ArgField::Lines { .. }
            | ast::ArgField::QueryComment { .. } => continue,
        }
    }

    if aggregate_to_array {
        sql.push_str(&format!(
            "{}group by {}.{}\n",
            indent_str, base_table_name, full_foreign_id
        ));
        // Add ORDER BY after GROUP BY to ensure deterministic ordering
        sql.push_str(&format!(
            "{}order by {}.{}\n",
            indent_str, base_table_name, full_foreign_id
        ));
    }
}

fn select_type(
    indent: usize,
    context: &typecheck::Context,
    column: &ast::Column,
    base_table_name: &str,
    query_field_name: &str,

    //
    sql: &mut String,
) {
    select_type_with_json_mode(
        indent,
        context,
        column,
        base_table_name,
        query_field_name,
        true,
        sql,
    );
}

fn select_type_final(
    indent: usize,
    context: &typecheck::Context,
    column: &ast::Column,
    base_table_name: &str,
    query_field_name: &str,

    //
    sql: &mut String,
) {
    select_type_with_json_mode(
        indent,
        context,
        column,
        base_table_name,
        query_field_name,
        false,
        sql,
    );
}

pub fn select_type_expression(
    indent: usize,
    context: &typecheck::Context,
    column: &ast::Column,
    base_table_name: &str,
    query_field_name: &str,
    use_jsonb: bool,
) -> String {
    let mut sql = String::new();
    select_type_with_json_mode(
        indent,
        context,
        column,
        base_table_name,
        query_field_name,
        use_jsonb,
        &mut sql,
    );
    sql
}

fn select_type_with_json_mode(
    indent: usize,
    context: &typecheck::Context,
    column: &ast::Column,
    base_table_name: &str,
    query_field_name: &str,
    use_jsonb: bool,
    sql: &mut String,
) {
    if column.type_.is_json_like() {
        let json_fn = if use_jsonb { "jsonb" } else { "json" };
        sql.push_str(&format!(
            "{}({}.{})",
            json_fn, base_table_name, query_field_name
        ));
        return;
    }

    // Handle boolean types: SQLite stores booleans as 0/1, convert to JSON boolean
    if column.type_.is_bool() {
        let json_fn = if use_jsonb { "jsonb" } else { "json" };
        sql.push_str(&format!(
            "{}(case when {}.{} = 1 then 'true' else 'false' end)",
            json_fn, base_table_name, query_field_name
        ));
        return;
    }

    // Look up custom types in the context
    let type_lookup = column
        .type_
        .get_custom_type_name()
        .and_then(|name| context.types.get(name));
    match type_lookup {
        None => sql.push_str(&format!("{}.{}", base_table_name, query_field_name)),
        Some((_, type_)) => match type_ {
            typecheck::Type::Integer => {
                sql.push_str(&format!("{}.{}", base_table_name, query_field_name))
            }
            typecheck::Type::Float => {
                sql.push_str(&format!("{}.{}", base_table_name, query_field_name))
            }
            typecheck::Type::String => {
                sql.push_str(&format!("{}.{}", base_table_name, query_field_name))
            }
            typecheck::Type::OneOf { variants, .. } => {
                let object_fn = if use_jsonb {
                    "jsonb_object"
                } else {
                    "json_object"
                };
                let indent_str = " ".repeat(indent);
                let when_indent = " ".repeat(indent + 2);
                let obj_indent = " ".repeat(indent + 4);
                let obj_field_indent = " ".repeat(indent + 6);
                sql.push_str(&format!("\n{}case\n", indent_str));
                for var in variants {
                    sql.push_str(&format!(
                        "{}when {}.{} = '{}' then",
                        when_indent, base_table_name, query_field_name, var.name
                    ));

                    match &var.fields {
                        None => sql.push_str(&format!(" {}('_type', '{}')\n", object_fn, var.name)),
                        Some(fields) => {
                            sql.push_str(&format!("\n{}{}(", obj_indent, object_fn));

                            sql.push_str(&format!(
                                "\n{}'_type', '{}',",
                                obj_field_indent, var.name
                            ));

                            let mut first_field = true;
                            for field in fields {
                                match field {
                                    ast::Field::Column(inner_column) => {
                                        if !first_field {
                                            sql.push_str(
                                                format!(",\n{}", obj_field_indent).as_str(),
                                            );
                                        } else {
                                            sql.push_str(
                                                format!("\n{}", obj_field_indent).as_str(),
                                            );
                                        }
                                        sql.push_str(&format!("'{}', ", inner_column.name,));

                                        select_type_with_json_mode(
                                            indent + 4,
                                            context,
                                            inner_column,
                                            base_table_name,
                                            &format!(
                                                "{}__{}",
                                                query_field_name, &inner_column.name
                                            ),
                                            use_jsonb,
                                            sql,
                                        );
                                        first_field = false;
                                    }
                                    _ => continue,
                                }
                            }
                            sql.push_str(&format!("\n{})\n", obj_indent));
                        }
                    }
                }
                sql.push_str(&format!("{}end", indent_str));
            }
            typecheck::Type::Record(_) => {
                sql.push_str(&format!("{}.{}", base_table_name, query_field_name))
            }
        },
    }
}

fn render_json_column(
    indent: usize,
    indent_str: &str,
    context: &typecheck::Context,
    base_table_name: &str,
    field_name: &str,
    column_name: &str,
    column: &ast::Column,
    first_field: &mut bool,
    sql: &mut String,
) {
    if !*first_field {
        sql.push_str(",\n");
    }

    sql.push_str(&format!("{}    '{}', ", indent_str, field_name));

    select_type(
        indent + 6,
        context,
        column,
        base_table_name,
        column_name,
        sql,
    );

    *first_field = false;
}

fn render_final_json_column(
    context: &typecheck::Context,
    indent: usize,
    indent_str: &str,
    base_table_name: &str,
    field_name: &str,
    column_name: &str,
    column: &ast::Column,
    first_field: &mut bool,
    sql: &mut String,
) {
    if !*first_field {
        sql.push_str(",\n");
    }

    sql.push_str(&format!("{}      '{}', ", indent_str, field_name));
    select_type_final(
        indent + 6,
        context,
        column,
        base_table_name,
        column_name,
        sql,
    );

    *first_field = false;
}

/*
This is slightly different than the others.

1. it should use `json`, not jsonb, because it's returning a final result



*/
fn final_select_formatted_as_json(
    indent: usize,
    context: &typecheck::Context,
    table: &typecheck::Table,
    query_table_field: &ast::QueryField,
    query_path: &str,

    //
    sql: &mut String,
) {
    let indent_str = " ".repeat(indent);

    let base_table_name = get_temp_table_name_for_path(query_path);
    let query_field_name = &query_table_field.name;

    // initial selection - wrap in JSON_GROUP_ARRAY (no outer json_object wrapper)
    sql.push_str(&format!(
        "\nselect\n{}  coalesce(json_group_array(\n",
        indent_str
    ));
    sql.push_str(&format!("{}    json_object(\n", indent_str));

    // Compose main json payload
    let mut first_field = true;
    let mut explicit_columns = HashSet::new();
    for field in &query_table_field.fields {
        if let ast::ArgField::Field(query_field) = field {
            if query_field.name == "*" {
                continue;
            }
            if let Some(ast::Field::Column(column)) = table
                .record
                .fields
                .iter()
                .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
            {
                explicit_columns.insert(column.name.clone());
            }
        }
    }

    let mut rendered_columns: HashSet<String> = HashSet::new();

    for field in &query_table_field.fields {
        match field {
            ast::ArgField::Field(query_field) => {
                if query_field.name == "*" {
                    for table_field in &table.record.fields {
                        if let ast::Field::Column(column) = table_field {
                            if explicit_columns.contains(&column.name) {
                                continue;
                            }
                            if rendered_columns.insert(column.name.clone()) {
                                render_final_json_column(
                                    context,
                                    indent,
                                    &indent_str,
                                    &base_table_name,
                                    &column.name,
                                    &column.name,
                                    column,
                                    &mut first_field,
                                    sql,
                                );
                            }
                        }
                    }
                    continue;
                }

                if let Some(table_field) = table
                    .record
                    .fields
                    .iter()
                    .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
                {
                    let aliased_field_name = ast::get_aliased_name(query_field);

                    match table_field {
                        ast::Field::Column(column) => {
                            if rendered_columns.insert(aliased_field_name.clone()) {
                                render_final_json_column(
                                    context,
                                    indent,
                                    &indent_str,
                                    &base_table_name,
                                    &aliased_field_name,
                                    &query_field.name,
                                    column,
                                    &mut first_field,
                                    sql,
                                );
                            }
                        }
                        ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
                            if !first_field {
                                sql.push_str(",\n");
                            }

                            if link_returns_singular_result(context, &table.record, link) {
                                // singular result, no need to coalesce
                                sql.push_str(&format!(
                                    "{}      '{}', temp__{}.{}",
                                    indent_str,
                                    aliased_field_name,
                                    query_field.name,
                                    aliased_field_name
                                ));
                                first_field = false;
                            } else {
                                // Coalesce as an empty array
                                // Use jsonb() to ensure type compatibility with jsonb_group_array result from temp tables
                                sql.push_str(&format!(
                                    "{}      '{}', coalesce(temp__{}.{}, jsonb('[]'))",
                                    indent_str,
                                    aliased_field_name,
                                    query_field.name,
                                    aliased_field_name
                                ));
                                first_field = false;
                            }
                        }
                        _ => continue,
                    }
                }
            }
            ast::ArgField::Arg(_)
            | ast::ArgField::Lines { .. }
            | ast::ArgField::QueryComment { .. } => continue,
        }
    }

    // Close json_object and json_group_array
    // Use COALESCE to handle empty results - json_group_array returns NULL when no rows
    // Use json() to ensure type compatibility with json_group_array result (not jsonb)
    sql.push_str(&format!(
        "\n{}    )\n{}  ), json('[]')) as {}\n{}from {}\n",
        indent_str, indent_str, query_field_name, indent_str, base_table_name
    ));

    // Join every link
    for field in &query_table_field.fields {
        match field {
            ast::ArgField::Field(query_field) => {
                if let Some(table_field) = table
                    .record
                    .fields
                    .iter()
                    .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
                {
                    match table_field {
                        ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
                            // let str = link.local_ids.join("");
                            // return vec![str];

                            // let query_json_table =
                            // format!("json__{}", ast::get_aliased_name(query_field));

                            if let Some(linked_table) = typecheck::get_linked_table(context, &link)
                            {
                                let linked_path = format!(
                                    "{}__{}",
                                    query_path,
                                    ast::get_aliased_name(query_field)
                                );
                                let query_temp_table =
                                    to_temp_table_alias(&linked_path, query_field, linked_table);

                                let local_temp_alias = format!("temp__{}", link.link_name);

                                sql.push_str(&format!(
                                    "{}  left join {} {} on {}.{} = {}.{}\n",
                                    indent_str,
                                    query_temp_table,
                                    local_temp_alias,
                                    local_temp_alias,
                                    link.foreign.fields.join(""),
                                    base_table_name,
                                    link.local_ids.join(""),
                                ));
                            }
                        }
                        _ => continue,
                    }
                }
            }
            ast::ArgField::Arg(_)
            | ast::ArgField::Lines { .. }
            | ast::ArgField::QueryComment { .. } => continue,
        }
    }

    // sql.push_str(&format!("{}group by {}\n", indent_str, full_foreign_id));
}

fn link_returns_singular_result(
    context: &typecheck::Context,
    local_record: &ast::RecordDetails,
    link: &ast::LinkDetails,
) -> bool {
    let primary_key_name = ast::get_primary_id_field_name(&local_record.fields);
    let is_one_to_many = link.local_ids.iter().all(|id| {
        primary_key_name
            .as_ref()
            .map(|pk| id == pk)
            .unwrap_or(false)
    });

    let linked_to_unique = if let Some(linked_table) = typecheck::get_linked_table(context, link) {
        ast::linked_to_unique_field_with_record(link, &linked_table.record)
    } else {
        ast::linked_to_unique_field(link)
    };

    !is_one_to_many && linked_to_unique
}

fn to_temp_table_alias(
    query_path: &str,
    query_field: &ast::QueryField,
    table: &typecheck::Table,
) -> String {
    if selects_for_link(query_field, table) {
        get_json_temp_table_name_for_path(query_path)
    } else {
        get_temp_table_name_for_path(query_path)
    }
}

// Field names

fn to_fieldnames(
    context: &typecheck::Context,
    table: &typecheck::Table,
    query_fields: &Vec<ast::ArgField>,
) -> Vec<String> {
    let mut selected_set = HashSet::new();

    for field in query_fields {
        match field {
            ast::ArgField::Field(query_field) => {
                if query_field.name == "*" {
                    for table_field in &table.record.fields {
                        if let ast::Field::Column(column) = table_field {
                            add_column_fieldnames(context, column, &mut selected_set);
                        }
                    }
                    continue;
                }

                if let Some(table_field) = table
                    .record
                    .fields
                    .iter()
                    .find(|&f| ast::has_field_or_linkname(&f, &query_field.name))
                {
                    add_concret_fieldnames(context, table_field, query_field, &mut selected_set);
                }
            }
            ast::ArgField::Arg(_)
            | ast::ArgField::Lines { .. }
            | ast::ArgField::QueryComment { .. } => continue,
        }
    }

    let mut selected: Vec<String> = selected_set.into_iter().collect();
    selected.sort_unstable();
    selected
}

fn add_concret_fieldnames(
    context: &typecheck::Context,
    table_field: &ast::Field,
    query_field: &ast::QueryField,
    selected_set: &mut HashSet<String>,
) {
    match table_field {
        ast::Field::Column(column) => {
            let query_field_name = query_field.name.clone();
            selected_set.insert(query_field_name.clone());

            // Look up custom types in the context
            let type_lookup = column
                .type_
                .get_custom_type_name()
                .and_then(|name| context.types.get(name));
            match type_lookup {
                None => (),
                Some((_, type_)) => match type_ {
                    typecheck::Type::OneOf { variants, .. } => {
                        for variant in variants {
                            match &variant.fields {
                                None => (),
                                Some(fields) => {
                                    for field in fields {
                                        match field {
                                            ast::Field::Column(inner_column) => {
                                                selected_set.insert(format!(
                                                    "{}__{}",
                                                    query_field_name, &inner_column.name
                                                ));
                                            }
                                            _ => (),
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => (),
                },
            };
        }
        ast::Field::FieldDirective(ast::FieldDirective::Link(link)) => {
            for local_id in &link.local_ids {
                selected_set.insert(local_id.clone());
            }
        }
        _ => (),
    }
}

fn add_column_fieldnames(
    context: &typecheck::Context,
    column: &ast::Column,
    selected_set: &mut HashSet<String>,
) {
    selected_set.insert(column.name.clone());

    let type_lookup = column
        .type_
        .get_custom_type_name()
        .and_then(|name| context.types.get(name));
    match type_lookup {
        None => (),
        Some((_, type_)) => match type_ {
            typecheck::Type::OneOf { variants, .. } => {
                for variant in variants {
                    match &variant.fields {
                        None => (),
                        Some(fields) => {
                            for field in fields {
                                match field {
                                    ast::Field::Column(inner_column) => {
                                        selected_set.insert(format!(
                                            "{}__{}",
                                            column.name, &inner_column.name
                                        ));
                                    }
                                    _ => (),
                                }
                            }
                        }
                    }
                }
            }
            _ => (),
        },
    };
}

/*
Querying things and then incrementally encoding

WITH
GamesFilter AS (
    SELECT id, name
    FROM games
    WHERE id = 1
),
PlayersForGames AS (
    SELECT
        p.game_id,
        p.id AS player_id,
        p.name AS player_name,
        p.points AS player_points,
        p.user_id
    FROM players p
    WHERE p.game_id IN (SELECT id FROM GamesFilter)
),
UsersForPlayers AS (
    SELECT
        pf.player_id,
        JSON_OBJECT(
            'userName', u.name
        ) AS user_json
    FROM PlayersForPlayers pf
    LEFT JOIN users u ON pf.user_id = u.id
),
FormattedPlayersWithUsers AS (
    SELECT
        game_id,
        JSON_GROUP_ARRAY(
            JSON_OBJECT(
                'id', pf.player_id,
                'aliasedName', pf.player_name,
                'points', pf.player_points,
                'user', COALESCE(uf.user_json, '{}')
            )
        ) AS players_json
    FROM PlayersForGames pf
    LEFT JOIN UsersForPlayers uf ON pf.player_id = uf.player_id
    GROUP BY game_id
)

SELECT
    JSON_OBJECT(
        'game', JSON_GROUP_ARRAY(
            JSON_OBJECT(
                'id', g.id,
                'name', g.name,
                'players', COALESCE(p.players_json, '[]')
            )
        )
    ) AS result
FROM GamesFilter g
LEFT JOIN FormattedPlayersWithUsers p ON g.id = p.game_id;



*/
