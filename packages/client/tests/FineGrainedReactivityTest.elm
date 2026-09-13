module FineGrainedReactivityTest exposing (suite)

import Data.Delta
import Data.Identity
import Data.QueryManager
import Data.Schema
import Data.Value exposing (Value)
import Db
import Db.Query
import Dict exposing (Dict)
import Expect
import Set exposing (Set)
import Test exposing (..)


suite : Test
suite =
    describe "Fine-Grained Query Reactivity"
        [ extractWhereClauseFieldsTests
        , doesChangeAffectWhereClauseTests
        , extractChangedRowIdsTests
        , shouldReExecuteQueryTests
        , integrationTests
        ]



-- Tests for extractWhereClauseFields


extractWhereClauseFieldsTests : Test
extractWhereClauseFieldsTests =
    describe "extractWhereClauseFields"
        [ test "extracts simple equality field" <|
            \_ ->
                let
                    whereClause =
                        Dict.fromList
                            [ ( "role", Db.Query.FilterValueSimple (Data.Value.StringValue "admin") )
                            ]

                    result =
                        Data.QueryManager.extractWhereClauseFields whereClause
                in
                Expect.equal (Set.fromList [ "role" ]) result
        , test "extracts multiple fields" <|
            \_ ->
                let
                    whereClause =
                        Dict.fromList
                            [ ( "role", Db.Query.FilterValueSimple (Data.Value.StringValue "admin") )
                            , ( "status", Db.Query.FilterValueSimple (Data.Value.StringValue "active") )
                            ]

                    result =
                        Data.QueryManager.extractWhereClauseFields whereClause
                in
                Expect.equal (Set.fromList [ "role", "status" ]) result
        , test "extracts fields from $and clause" <|
            \_ ->
                let
                    whereClause =
                        Dict.fromList
                            [ ( "$and"
                              , Db.Query.FilterValueAnd
                                    [ Dict.fromList [ ( "role", Db.Query.FilterValueSimple (Data.Value.StringValue "admin") ) ]
                                    , Dict.fromList [ ( "status", Db.Query.FilterValueSimple (Data.Value.StringValue "active") ) ]
                                    ]
                              )
                            ]

                    result =
                        Data.QueryManager.extractWhereClauseFields whereClause
                in
                Expect.equal (Set.fromList [ "role", "status" ]) result
        , test "extracts fields from $or clause" <|
            \_ ->
                let
                    whereClause =
                        Dict.fromList
                            [ ( "$or"
                              , Db.Query.FilterValueOr
                                    [ Dict.fromList [ ( "role", Db.Query.FilterValueSimple (Data.Value.StringValue "admin") ) ]
                                    , Dict.fromList [ ( "role", Db.Query.FilterValueSimple (Data.Value.StringValue "moderator") ) ]
                                    ]
                              )
                            ]

                    result =
                        Data.QueryManager.extractWhereClauseFields whereClause
                in
                Expect.equal (Set.fromList [ "role" ]) result
        , test "extracts fields from operator filters" <|
            \_ ->
                let
                    whereClause =
                        Dict.fromList
                            [ ( "age"
                              , Db.Query.FilterValueOperators
                                    (Dict.fromList
                                        [ ( "$gte", Db.Query.FilterValueSimple (Data.Value.IntValue 18) )
                                        ]
                                    )
                              )
                            ]

                    result =
                        Data.QueryManager.extractWhereClauseFields whereClause
                in
                Expect.equal (Set.fromList [ "age" ]) result
        , test "handles nested $and and $or" <|
            \_ ->
                let
                    whereClause =
                        Dict.fromList
                            [ ( "$and"
                              , Db.Query.FilterValueAnd
                                    [ Dict.fromList [ ( "status", Db.Query.FilterValueSimple (Data.Value.StringValue "active") ) ]
                                    , Dict.fromList
                                        [ ( "$or"
                                          , Db.Query.FilterValueOr
                                                [ Dict.fromList [ ( "role", Db.Query.FilterValueSimple (Data.Value.StringValue "admin") ) ]
                                                , Dict.fromList [ ( "priority", Db.Query.FilterValueSimple (Data.Value.IntValue 1) ) ]
                                                ]
                                          )
                                        ]
                                    ]
                              )
                            ]

                    result =
                        Data.QueryManager.extractWhereClauseFields whereClause
                in
                Expect.equal (Set.fromList [ "status", "role", "priority" ]) result
        ]



-- Tests for doesChangeAffectWhereClause


doesChangeAffectWhereClauseTests : Test
doesChangeAffectWhereClauseTests =
    describe "doesChangeAffectWhereClause"
        [ test "returns True when filtered field changes" <|
            \_ ->
                let
                    whereClause =
                        Dict.fromList
                            [ ( "role", Db.Query.FilterValueSimple (Data.Value.StringValue "admin") )
                            ]

                    oldRow =
                        Dict.fromList
                            [ ( "id", Data.Value.IntValue 1 )
                            , ( "role", Data.Value.StringValue "user" )
                            , ( "email", Data.Value.StringValue "old@example.com" )
                            ]

                    newRow =
                        Dict.fromList
                            [ ( "id", Data.Value.IntValue 1 )
                            , ( "role", Data.Value.StringValue "admin" )
                            , ( "email", Data.Value.StringValue "old@example.com" )
                            ]

                    result =
                        Data.QueryManager.doesChangeAffectWhereClause whereClause oldRow newRow
                in
                Expect.equal True result
        , test "returns False when non-filtered field changes" <|
            \_ ->
                let
                    whereClause =
                        Dict.fromList
                            [ ( "role", Db.Query.FilterValueSimple (Data.Value.StringValue "admin") )
                            ]

                    oldRow =
                        Dict.fromList
                            [ ( "id", Data.Value.IntValue 1 )
                            , ( "role", Data.Value.StringValue "admin" )
                            , ( "email", Data.Value.StringValue "old@example.com" )
                            ]

                    newRow =
                        Dict.fromList
                            [ ( "id", Data.Value.IntValue 1 )
                            , ( "role", Data.Value.StringValue "admin" )
                            , ( "email", Data.Value.StringValue "new@example.com" )
                            ]

                    result =
                        Data.QueryManager.doesChangeAffectWhereClause whereClause oldRow newRow
                in
                Expect.equal False result
        , test "returns False when filtered field stays same" <|
            \_ ->
                let
                    whereClause =
                        Dict.fromList
                            [ ( "role", Db.Query.FilterValueSimple (Data.Value.StringValue "admin") )
                            , ( "status", Db.Query.FilterValueSimple (Data.Value.StringValue "active") )
                            ]

                    oldRow =
                        Dict.fromList
                            [ ( "id", Data.Value.IntValue 1 )
                            , ( "role", Data.Value.StringValue "admin" )
                            , ( "status", Data.Value.StringValue "active" )
                            , ( "email", Data.Value.StringValue "old@example.com" )
                            ]

                    newRow =
                        Dict.fromList
                            [ ( "id", Data.Value.IntValue 1 )
                            , ( "role", Data.Value.StringValue "admin" )
                            , ( "status", Data.Value.StringValue "active" )
                            , ( "email", Data.Value.StringValue "new@example.com" )
                            ]

                    result =
                        Data.QueryManager.doesChangeAffectWhereClause whereClause oldRow newRow
                in
                Expect.equal False result
        , test "returns True when any filtered field changes" <|
            \_ ->
                let
                    whereClause =
                        Dict.fromList
                            [ ( "role", Db.Query.FilterValueSimple (Data.Value.StringValue "admin") )
                            , ( "status", Db.Query.FilterValueSimple (Data.Value.StringValue "active") )
                            ]

                    oldRow =
                        Dict.fromList
                            [ ( "id", Data.Value.IntValue 1 )
                            , ( "role", Data.Value.StringValue "admin" )
                            , ( "status", Data.Value.StringValue "active" )
                            ]

                    newRow =
                        Dict.fromList
                            [ ( "id", Data.Value.IntValue 1 )
                            , ( "role", Data.Value.StringValue "admin" )
                            , ( "status", Data.Value.StringValue "inactive" )
                            ]

                    result =
                        Data.QueryManager.doesChangeAffectWhereClause whereClause oldRow newRow
                in
                Expect.equal True result
        ]



-- Tests for extractChangedRowIds


extractChangedRowIdsTests : Test
extractChangedRowIdsTests =
    describe "extractChangedRowIds"
        [ test "extracts row IDs from single table delta" <|
            \_ ->
                let
                    delta =
                        { tableGroups =
                            [ { tableName = "users"
                              , headers = [ "id", "name" ]
                              , rows =
                                    [ [ Data.Value.IntValue 1, Data.Value.StringValue "Alice" ]
                                    , [ Data.Value.IntValue 2, Data.Value.StringValue "Bob" ]
                                    ]
                              }
                            ]
                        }

                    result =
                        Data.QueryManager.extractChangedRowIds identitySchema delta
                in
                Expect.equal
                    (Ok (Dict.fromList [ ( "users", keys [ 1, 2 ] ) ]))
                    result
        , test "extracts row IDs from multiple tables" <|
            \_ ->
                let
                    delta =
                        { tableGroups =
                            [ { tableName = "users"
                              , headers = [ "id", "name" ]
                              , rows = [ [ Data.Value.IntValue 1, Data.Value.StringValue "Alice" ] ]
                              }
                            , { tableName = "posts"
                              , headers = [ "id", "title" ]
                              , rows =
                                    [ [ Data.Value.IntValue 10, Data.Value.StringValue "Post 1" ]
                                    , [ Data.Value.IntValue 11, Data.Value.StringValue "Post 2" ]
                                    ]
                              }
                            ]
                        }

                    result =
                        Data.QueryManager.extractChangedRowIds identitySchema delta
                in
                Expect.equal
                    (Ok
                        (Dict.fromList
                            [ ( "users", keys [ 1 ] )
                            , ( "posts", keys [ 10, 11 ] )
                            ]
                        )
                    )
                    result
        , test "handles empty delta" <|
            \_ ->
                let
                    delta =
                        { tableGroups = [] }

                    result =
                        Data.QueryManager.extractChangedRowIds identitySchema delta
                in
                Expect.equal (Ok Dict.empty) result
        , test "rejects the whole delta for an invalid ID" <|
            \_ ->
                let
                    delta =
                        { tableGroups =
                            [ { tableName = "users"
                              , headers = [ "id", "name" ]
                              , rows =
                                    [ [ Data.Value.IntValue 1, Data.Value.StringValue "Alice" ]
                                    , [ Data.Value.StringValue "invalid", Data.Value.StringValue "Bob" ]
                                    , [ Data.Value.IntValue 3, Data.Value.StringValue "Charlie" ]
                                    ]
                              }
                            ]
                        }

                    result =
                        Data.QueryManager.extractChangedRowIds identitySchema delta
                in
                Expect.equal
                    (Err "Primary key has the wrong type")
                    result
        ]



-- Tests for shouldReExecuteQuery


shouldReExecuteQueryTests : Test
shouldReExecuteQueryTests =
    describe "shouldReExecuteQuery"
        [ test "returns NoReExecute when tables don't overlap" <|
            \_ ->
                let
                    schema =
                        { tables = identitySchema.tables
                        , queryFieldToTable = Dict.fromList [ ( "users", "users" ) ]
                        }

                    subscription =
                        { queryId = "q1"
                        , query =
                            Dict.fromList
                                [ ( "users"
                                  , { selections = Dict.empty
                                    , where_ = Nothing
                                    , sort = Nothing
                                    , limit = Nothing
                                    }
                                  )
                                ]
                        , input = Data.Value.NullValue |> Data.Value.encodeValue
                        , callbackPort = "port1"
                        , resultRowIds = Dict.fromList [ ( "users", keys [ 1, 2, 3 ] ) ]
                        , revision = 0
                        , lastResult = Nothing
                        }

                    delta =
                        { tableGroups =
                            [ { tableName = "posts"
                              , headers = [ "id", "title" ]
                              , rows = [ [ Data.Value.IntValue 10, Data.Value.StringValue "Post" ] ]
                              }
                            ]
                        }

                    db =
                        Db.init schema

                    result =
                        Data.QueryManager.shouldReExecuteQuery schema db subscription delta
                in
                Expect.equal Data.QueryManager.NoReExecute result
        , test "returns ReExecuteFull when new rows appear (potential inserts)" <|
            \_ ->
                let
                    schema =
                        { tables = identitySchema.tables
                        , queryFieldToTable = Dict.fromList [ ( "users", "users" ) ]
                        }

                    subscription =
                        { queryId = "q1"
                        , query =
                            Dict.fromList
                                [ ( "users"
                                  , { selections = Dict.empty
                                    , where_ = Nothing
                                    , sort = Nothing
                                    , limit = Nothing
                                    }
                                  )
                                ]
                        , input = Data.Value.NullValue |> Data.Value.encodeValue
                        , callbackPort = "port1"
                        , resultRowIds = Dict.fromList [ ( "users", keys [ 1, 2, 3 ] ) ]
                        , revision = 0
                        , lastResult = Nothing
                        }

                    delta =
                        { tableGroups =
                            [ { tableName = "users"
                              , headers = [ "id", "name" ]
                              , rows = [ [ Data.Value.IntValue 999, Data.Value.StringValue "Charlie" ] ]
                              }
                            ]
                        }

                    db =
                        Db.init schema

                    result =
                        Data.QueryManager.shouldReExecuteQuery schema db subscription delta
                in
                -- Should re-execute because row 999 is new (might need to be included)
                Expect.equal Data.QueryManager.ReExecuteFull result
        , test "returns ReExecuteFull when nested one-to-many child table changes" <|
            \_ ->
                let
                    schema =
                        { tables =
                            Dict.fromList
                                [ ( "games"
                                  , { name = "games"
                                    , primaryKey = { name = "id", kind = Data.Schema.IntKey }
                                    , links =
                                        Dict.fromList
                                            [ ( "gameMembers"
                                              , { type_ = Data.Schema.OneToMany
                                                , from = "id"
                                                , to =
                                                    { table = "game_members"
                                                    , column = "gameId"
                                                    }
                                                }
                                              )
                                            ]
                                    , indices = []
                                    }
                                  )
                                , ( "game_members"
                                  , { name = "game_members"
                                    , primaryKey = { name = "id", kind = Data.Schema.IntKey }
                                    , links = Dict.empty
                                    , indices = []
                                    }
                                  )
                                ]
                        , queryFieldToTable = Dict.fromList [ ( "game", "games" ) ]
                        }

                    nestedSelection =
                        { selections = Dict.fromList [ ( "id", Db.Query.SelectField Nothing ) ]
                        , where_ = Nothing
                        , sort = Nothing
                        , limit = Nothing
                        }

                    subscription =
                        { queryId = "q-games"
                        , query =
                            Dict.fromList
                                [ ( "game"
                                  , { selections =
                                        Dict.fromList
                                            [ ( "id", Db.Query.SelectField Nothing )
                                            , ( "name", Db.Query.SelectField Nothing )
                                            , ( "gameMembers", Db.Query.SelectNested Nothing nestedSelection )
                                            ]
                                    , where_ = Nothing
                                    , sort = Nothing
                                    , limit = Nothing
                                    }
                                  )
                                ]
                        , input = Data.Value.NullValue |> Data.Value.encodeValue
                        , callbackPort = "port1"
                        , resultRowIds = Dict.fromList [ ( "game", keys [ 1 ] ) ]
                        , revision = 1
                        , lastResult = Nothing
                        }

                    delta =
                        { tableGroups =
                            [ { tableName = "game_members"
                              , headers = [ "id", "gameId", "userId" ]
                              , rows = [ [ Data.Value.IntValue 10, Data.Value.IntValue 1, Data.Value.IntValue 2 ] ]
                              }
                            ]
                        }

                    db =
                        Db.init schema

                    result =
                        Data.QueryManager.shouldReExecuteQuery schema db subscription delta
                in
                Expect.equal Data.QueryManager.ReExecuteFull result
        , test "returns ReExecuteFull when new rows added (potential inserts)" <|
            \_ ->
                let
                    schema =
                        { tables = identitySchema.tables
                        , queryFieldToTable = Dict.fromList [ ( "users", "users" ) ]
                        }

                    subscription =
                        { queryId = "q1"
                        , query =
                            Dict.fromList
                                [ ( "users"
                                  , { selections = Dict.empty
                                    , where_ =
                                        Just
                                            (Dict.fromList
                                                [ ( "role", Db.Query.FilterValueSimple (Data.Value.StringValue "admin") )
                                                ]
                                            )
                                    , sort = Nothing
                                    , limit = Nothing
                                    }
                                  )
                                ]
                        , input = Data.Value.NullValue |> Data.Value.encodeValue
                        , callbackPort = "port1"
                        , resultRowIds = Dict.fromList [ ( "users", keys [ 1, 2, 3 ] ) ]
                        , revision = 0
                        , lastResult = Nothing
                        }

                    delta =
                        { tableGroups =
                            [ { tableName = "users"
                              , headers = [ "id", "name", "role" ]
                              , rows =
                                    [ [ Data.Value.IntValue 999, Data.Value.StringValue "New User", Data.Value.StringValue "admin" ]
                                    ]
                              }
                            ]
                        }

                    db =
                        Db.init schema

                    result =
                        Data.QueryManager.shouldReExecuteQuery schema db subscription delta
                in
                Expect.equal Data.QueryManager.ReExecuteFull result
        ]



-- Integration tests


integrationTests : Test
integrationTests =
    describe "Integration: Full query reactivity flow"
        [ test "query with WHERE clause re-executes when selected non-filtered field changes" <|
            \_ ->
                let
                    -- Schema setup
                    schema =
                        { tables = identitySchema.tables
                        , queryFieldToTable = Dict.fromList [ ( "users", "users" ) ]
                        }

                    -- Query: users WHERE role = 'admin'
                    subscription =
                        { queryId = "admin-query"
                        , query =
                            Dict.fromList
                                [ ( "users"
                                  , { selections = Dict.empty
                                    , where_ =
                                        Just
                                            (Dict.fromList
                                                [ ( "role", Db.Query.FilterValueSimple (Data.Value.StringValue "admin") )
                                                ]
                                            )
                                    , sort = Nothing
                                    , limit = Nothing
                                    }
                                  )
                                ]
                        , input = Data.Value.NullValue |> Data.Value.encodeValue
                        , callbackPort = "port1"
                        , resultRowIds = Dict.fromList [ ( "users", keys [ 1, 2 ] ) ]
                        , revision = 0
                        , lastResult = Nothing
                        }

                    -- DB state: users 1 and 2 are admins
                    db =
                        { tables =
                            Dict.fromList
                                [ ( "users"
                                  , Dict.fromList
                                        [ ( Data.Identity.int 1
                                          , Dict.fromList
                                                [ ( "id", Data.Value.IntValue 1 )
                                                , ( "role", Data.Value.StringValue "admin" )
                                                , ( "email", Data.Value.StringValue "admin1@example.com" )
                                                ]
                                          )
                                        , ( Data.Identity.int 2
                                          , Dict.fromList
                                                [ ( "id", Data.Value.IntValue 2 )
                                                , ( "role", Data.Value.StringValue "admin" )
                                                , ( "email", Data.Value.StringValue "admin2@example.com" )
                                                ]
                                          )
                                        ]
                                  )
                                ]
                        , indices = Dict.empty
                        , schema = schema
                        }

                    -- Delta: user 1 changes email (NOT role)
                    delta =
                        { tableGroups =
                            [ { tableName = "users"
                              , headers = [ "id", "role", "email" ]
                              , rows =
                                    [ [ Data.Value.IntValue 1
                                      , Data.Value.StringValue "admin"
                                      , Data.Value.StringValue "newemail@example.com"
                                      ]
                                    ]
                              }
                            ]
                        }

                    result =
                        Data.QueryManager.shouldReExecuteQuery schema db subscription delta
                in
                Expect.equal Data.QueryManager.ReExecuteFull result
        , test "query with WHERE clause triggers re-execution when filtered field changes" <|
            \_ ->
                let
                    schema =
                        { tables = identitySchema.tables
                        , queryFieldToTable = Dict.fromList [ ( "users", "users" ) ]
                        }

                    subscription =
                        { queryId = "admin-query"
                        , query =
                            Dict.fromList
                                [ ( "users"
                                  , { selections = Dict.empty
                                    , where_ =
                                        Just
                                            (Dict.fromList
                                                [ ( "role", Db.Query.FilterValueSimple (Data.Value.StringValue "admin") )
                                                ]
                                            )
                                    , sort = Nothing
                                    , limit = Nothing
                                    }
                                  )
                                ]
                        , input = Data.Value.NullValue |> Data.Value.encodeValue
                        , callbackPort = "port1"
                        , resultRowIds = Dict.fromList [ ( "users", keys [ 1, 2 ] ) ]
                        , revision = 0
                        , lastResult = Nothing
                        }

                    db =
                        { tables =
                            Dict.fromList
                                [ ( "users"
                                  , Dict.fromList
                                        [ ( Data.Identity.int 1
                                          , Dict.fromList
                                                [ ( "id", Data.Value.IntValue 1 )
                                                , ( "role", Data.Value.StringValue "admin" )
                                                ]
                                          )
                                        ]
                                  )
                                ]
                        , indices = Dict.empty
                        , schema = schema
                        }

                    -- Delta: user 1 changes role from admin to user
                    delta =
                        { tableGroups =
                            [ { tableName = "users"
                              , headers = [ "id", "role" ]
                              , rows =
                                    [ [ Data.Value.IntValue 1
                                      , Data.Value.StringValue "user"
                                      ]
                                    ]
                              }
                            ]
                        }

                    result =
                        Data.QueryManager.shouldReExecuteQuery schema db subscription delta
                in
                -- Should re-execute because 'role' changed
                Expect.equal Data.QueryManager.ReExecuteFull result
        ]


keys : List Int -> Set.Set Data.Identity.Key
keys =
    List.map Data.Identity.int >> Set.fromList


identitySchema : Data.Schema.SchemaMetadata
identitySchema =
    { tables =
        [ "users", "posts" ]
            |> List.map (\name -> ( name, { name = name, primaryKey = { name = "id", kind = Data.Schema.IntKey }, links = Dict.empty, indices = [] } ))
            |> Dict.fromList
    , queryFieldToTable = Dict.fromList [ ( "users", "users" ), ( "posts", "posts" ) ]
    }
