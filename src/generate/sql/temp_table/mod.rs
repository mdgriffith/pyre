/*  Legacy temporary-table approach for nested inserts.

    Remote Turso does not support temporary tables. Keep this module only until
    nested inserts use RETURNING result bindings or pre-assigned UUID/ULID IDs.

This is because SQLite does not allow nested inserts for CTEs.


For this mutation:

    insert UserNew($name: String) {
        user {
            name = $name
            status = Active
            accounts {
                name = "My account"
                status = "Untyped status"
            }
            posts {
                title = "My first post"
                content = "This is my first post"
                status = Active
            }
            databaseUsers {
                databaseId = "user.db"
            }
        }
    }

I was generating this code, which is CTE based:

    with inserted_user as (
        insert into users (name, status)
        values ($name, 'Active')
        returning *
    ), inserted_accounts as (
        insert into accounts (userId, name, status)
        select "inserted_user"."id", 'My account', 'Untyped status'
        from inserted_user
        returning *
    ), inserted_posts as (
        insert into posts (authorUserId, title, content, status)
        select "inserted_user"."id", 'My first post', 'This is my first post', 'Active'
        from inserted_user
        returning *
    ), inserted_databaseUsers as (
        insert into databaseUsers (userId, databaseId)
        select "inserted_user"."id", 'user.db'
        from inserted_user
        returning *
    )
    select
        "inserted_user"."name" as "user__name",
        "inserted_user"."status" as "user__status",
        "inserted_accounts"."name" as "accounts__name",
        "inserted_accounts"."status" as "accounts__status",
        "inserted_posts"."title" as "posts__title",
        "inserted_posts"."content" as "posts__content",
        "inserted_posts"."status" as "posts__status",
        "inserted_databaseUsers"."databaseId" as "databaseUsers__databaseId"
    from
        "inserted_user"
        left join "inserted_databaseUsers" on "inserted_user"."id" = "inserted_databaseUsers"."userId"
        left join "inserted_posts" on "inserted_user"."id" = "inserted_posts"."authorUserId"
        left join "inserted_accounts" on "inserted_user"."id" = "inserted_accounts"."userId"
    ;

Clean beans, yeah?

Except SQLite don't wanna do it.

Instead, we can generate this query.

    -- Create a temporary table to store the user ID
    create temporary table temp_ids (userId integer);

    -- Insert into the users table and store the ID in the temporary table
    insert into users (name, status)
    values ($name, 'Active');

    insert into temp_ids (userId)
    values (last_insert_rowid());

    -- Insert into the accounts table using the user ID from the temporary table
    insert into accounts (userId, name, status)
    select userId, 'My account', 'Untyped status'
    from temp_ids;

    -- Insert into the posts table using the user ID from the temporary table
    insert into posts (authorUserId, title, content, status)
    select userId, 'My first post', 'This is my first post', 'Active'
    from temp_ids;

    -- Insert into the databaseUsers table using the user ID from the temporary table
    insert into databaseUsers (userId, databaseId)
    select userId, 'user.db'
    from temp_ids;

    -- Retrieve the joined data
    select
        u.name as user__name,
        u.status as user__status,
        a.name as accounts__name,
        a.status as accounts__status,
        p.title as posts__title,
        p.content as posts__content,
        p.status as posts__status,
        d.databaseId as databaseUsers__databaseId
    from
        users u
    join temp_ids t on u.id = t.userId
    left join accounts a on u.id = a.userId
    left join posts p on u.id = p.authorUserId
    left join databaseUsers d on u.id = d.userId;

    -- Drop the temporary table when done
    drop table temp_ids;


Which, yeah, ain't too bad.  We're inserting our one dependency, and then storing that in a temp table.

The temp table is because we can't call `last_insert_rowid()` each time, because that's gonna change.


If there are multiple deps, then multiple temp tables will be needed.



*/

pub mod insert;
