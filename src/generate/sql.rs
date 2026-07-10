pub mod cte;
pub mod delete;
pub mod json;
pub mod returning;
pub mod select;
pub mod temp_table;
pub mod to_sql;

use crate::ast;
use crate::typecheck;

/*
Exmple query Generation:

    query Init($userId: Int) {
        user {
            @limit 1
            @where id = $userId
            id
            name
            email
            games: gameMembers {
                admin
                game {
                    id
                    name
                }
            }
        }
    }

SQL, CTE Form

    WITH user_info AS (
        SELECT
            user.id AS user_id,
            user.name AS user_name,
            user.email AS user_email
        FROM user
        WHERE user.id = $userId
        LIMIT 1
    ),
    game_members_info AS (
        SELECT
            gameMembers.user_id AS gm_user_id,
            gameMembers.admin AS gm_admin,
            game.id AS game_id,
            game.name AS game_name
        FROM gameMembers
        JOIN game ON game.id = gameMembers.game_id
    )
    SELECT
        ui.user_id AS id,
        ui.user_name AS name,
        ui.user_email AS email,
        gmi.gm_admin AS admin,
        gmi.game_id AS "game.id",
        gmi.game_name AS "game.name"
    FROM user_info ui
    LEFT JOIN game_members_info gmi ON ui.user_id = gmi.gm_user_id;


This uses CTE (Common Table Expressions)


SQL, Join Form

    SELECT
        user.id AS id,
        user.name AS name,
        user.email AS email,
        gameMembers.admin AS admin,
        game.id AS "game.id",
        game.name AS "game.name"
    FROM user
    LEFT JOIN gameMembers ON user.id = gameMembers.user_id
    LEFT JOIN game ON game.id = gameMembers.game_id
    WHERE user.id = $userId
    LIMIT 1;


And there is also Batch form, where we execute separate sql queries.


    SELECT
        user.id AS id,
        user.name AS name,
        user.email AS email
    FROM user
    WHERE user.id = $userId
    LIMIT 1;

    SELECT
        gameMembers.admin AS admin,
        game.id AS "game.id",
        game.name AS "game.name"
    FROM gameMembers
    JOIN game ON game.id = gameMembers.game_id
    WHERE gameMembers.user_id = $userId;




We generally only need to use something other than the standard join approach when there is
a limit. In that case, we need to use a CTE or a batched appraoch to get the limit and then join the other tables.



*/

//  QUERIES
//
pub fn to_string(
    context: &typecheck::Context,
    query: &ast::Query,
    query_info: &typecheck::QueryInfo,
    table: &typecheck::Table,
    table_field: &ast::QueryField,
) -> Vec<to_sql::Prepared> {
    to_string_with_affected_rows(context, query, query_info, table, table_field, true)
}

pub fn to_string_with_affected_rows(
    context: &typecheck::Context,
    query: &ast::Query,
    query_info: &typecheck::QueryInfo,
    table: &typecheck::Table,
    table_field: &ast::QueryField,
    include_affected_rows: bool,
) -> Vec<to_sql::Prepared> {
    match query.operation {
        ast::QueryOperation::Query => {
            json::select::select_to_string(context, query, query_info, table, table_field)
        }
        ast::QueryOperation::Insert =>
        // crate::generate::sql::cte::insert::insert_to_string(
        //     context,
        //     query,
        //     query_info,
        //     table,
        //     table_field,
        // ),
        {
            crate::generate::sql::temp_table::insert::insert_to_string(
                context,
                query,
                query_info,
                table,
                table_field,
                include_affected_rows,
            )
        }
        ast::QueryOperation::Update => crate::generate::sql::cte::update::update_to_string(
            context,
            query,
            query_info,
            table,
            table_field,
            include_affected_rows,
        ),

        ast::QueryOperation::Delete => delete::delete_to_string(
            context,
            query_info,
            table,
            table_field,
            include_affected_rows,
        ),
    }
}
