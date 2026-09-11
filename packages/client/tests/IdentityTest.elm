module IdentityTest exposing (suite)

import Data.Catchup as Catchup
import Data.Delta as Delta
import Data.Error
import Data.Identity as Identity
import Data.QueryManager as QueryManager
import Data.Schema as Schema
import Data.Value exposing (Value(..))
import Db
import Db.Index
import Db.Query as Query
import Dict
import Expect
import Json.Decode as Decode
import Json.Encode as Encode
import Set
import Test exposing (describe, test)


schema : Schema.SchemaMetadata
schema =
    { tables =
        Dict.fromList
            [ ( "people"
              , { name = "people"
                , primaryKey = { name = "personKey", kind = Schema.UuidKey }
                , indices = [ { field = "name", unique = False, primary = False } ]
                , links = Dict.fromList [ ( "notes", { type_ = Schema.OneToMany, from = "personKey", to = { table = "notes", column = "owner" } } ) ]
                }
              )
            , ( "notes"
              , { name = "notes"
                , primaryKey = { name = "number", kind = Schema.IntKey }
                , indices = [ { field = "owner", unique = False, primary = False } ]
                , links =
                    Dict.fromList
                        [ ( "person", { type_ = Schema.ManyToOne, from = "owner", to = { table = "people", column = "personKey" } } )
                        , ( "byName", { type_ = Schema.OneToOne, from = "label", to = { table = "people", column = "name" } } )
                        ]
                }
              )
            ]
    , queryFieldToTable = Dict.fromList [ ( "person", "people" ), ( "note", "notes" ), ( "otherNotes", "notes" ) ]
    }


alice : String
alice =
    "00000000-0000-4000-8000-000000000001"


bob : String
bob =
    "00000000-0000-4000-8000-000000000002"


initial =
    { tables =
        Dict.fromList
            [ ( "people"
              , [ Dict.fromList [ ( "personKey", StringValue alice ), ( "name", StringValue "Alice" ), ( "updatedAt", IntValue 5 ) ]
                , Dict.fromList [ ( "personKey", StringValue bob ), ( "name", StringValue "Bob" ), ( "updatedAt", IntValue 5 ) ]
                ]
              )
            , ( "notes", List.map (\n -> Dict.fromList [ ( "number", IntValue n ), ( "owner", StringValue alice ), ( "label", StringValue "Alice" ), ( "updatedAt", IntValue 5 ) ]) [ 10, 2 ] )
            ]
    , cursor = Dict.empty
    , databaseEpoch = Nothing
    , lastAppliedServerRevision = Nothing
    }


withDb check =
    case Db.fromInitialData schema initial of
        Err error ->
            Expect.fail error

        Ok db ->
            check db


field selections =
    { selections = Dict.fromList selections, where_ = Nothing, sort = Nothing, limit = Nothing }


projection =
    Dict.fromList [ ( "note", field [ ( "id", Query.SelectField (Just "label") ) ] ) ]


subscription db query =
    let
        result =
            Db.executeQueryWithTracking schema db query
    in
    { queryId = "q"
    , query = query
    , input = Encode.null
    , callbackPort = "unused"
    , resultRowIds = result.rowIds
    , revision = 1
    , lastResult = Just result.results
    }


noteDelta headers rows =
    { tableGroups = [ { tableName = "notes", headers = headers, rows = rows } ] }


suite =
    describe "Schema-driven identity"
        [ test "integer non-id keys retain numeric order and UUID keys retain raw wire identity" <|
            \_ ->
                withDb <|
                    \db ->
                        Expect.equal
                            ( [ Identity.int 2, Identity.int 10 ], Just (StringValue alice) )
                            ( Dict.get "notes" db.tables |> Maybe.map Dict.keys |> Maybe.withDefault []
                            , Dict.get "people" db.tables |> Maybe.andThen (Dict.get (Identity.uuid alice)) |> Maybe.andThen (Dict.get "personKey")
                            )
        , test "query tracking uses source identity before projections and aliases" <|
            \_ ->
                withDb <|
                    \db ->
                        let
                            result =
                                Db.executeQueryWithTracking schema db projection
                        in
                        Expect.equal
                            ( Dict.fromList [ ( "note", Set.fromList [ Identity.int 2, Identity.int 10 ] ) ]
                            , Dict.fromList [ ( "note", [ Dict.singleton "id" (StringValue "Alice"), Dict.singleton "id" (StringValue "Alice") ] ) ]
                            )
                            ( result.rowIds, result.results )
        , test "UUID one-to-many and many-to-one resolve non-id keys without projecting identities" <|
            \_ ->
                withDb <|
                    \db ->
                        let
                            nested =
                                field [ ( "person", Query.SelectNested Nothing (field [ ( "name", Query.SelectField Nothing ) ]) ) ]

                            query =
                                Dict.singleton "person" (field [ ( "children", Query.SelectNested (Just "notes") nested ) ])

                            child =
                                ObjectValue (Dict.singleton "person" (ObjectValue (Dict.singleton "name" (StringValue "Alice"))))
                        in
                        Expect.equal
                            (Dict.singleton "person" [ Dict.singleton "children" (ArrayValue [ child, child ]), Dict.singleton "children" (ArrayValue []) ])
                            (Db.executeQuery schema db query)
        , test "one-to-one honors a declared non-primary target column" <|
            \_ ->
                withDb <|
                    \db ->
                        let
                            query =
                                Dict.singleton "note" (field [ ( "byName", Query.SelectNested Nothing (field [ ( "name", Query.SelectField Nothing ) ]) ) ])
                        in
                        Expect.equal
                            (Dict.singleton "note" (List.repeat 2 (Dict.singleton "byName" (ObjectValue (Dict.singleton "name" (StringValue "Alice"))))))
                            (Db.executeQuery schema db query)
        , test "delta header order is independent of identity position and repeated groups merge tracking" <|
            \_ ->
                let
                    group n =
                        { tableName = "notes", headers = [ "label", "number" ], rows = [ [ StringValue "changed", IntValue n ] ] }
                in
                Expect.equal (Ok (Dict.singleton "notes" (Set.fromList [ Identity.int 2, Identity.int 10 ])))
                    (QueryManager.extractChangedRowIds schema { tableGroups = [ group 2, group 10 ] })
        , test "UUID delta updates indexes and relationship membership atomically" <|
            \_ ->
                withDb <|
                    \db ->
                        let
                            delta =
                                noteDelta [ "owner", "number", "label" ] [ [ StringValue bob, IntValue 2, StringValue "moved" ] ]

                            query =
                                Dict.singleton "note" { selections = Dict.empty, where_ = Just (Dict.singleton "owner" (Query.FilterValueSimple (StringValue bob))), sort = Nothing, limit = Nothing }
                        in
                        Db.applyDelta delta db
                            |> Result.map (\next -> (Db.executeQueryWithTracking schema next query).rowIds)
                            |> Expect.equal (Ok (Dict.singleton "note" (Set.singleton (Identity.int 2))))
        , test "UUID row upsert uses schema key rather than an arbitrary integer id column" <|
            \_ ->
                withDb <|
                    \db ->
                        Db.applyDelta { tableGroups = [ { tableName = "people", headers = [ "id", "personKey", "name" ], rows = [ [ IntValue 999, StringValue alice, StringValue "Renamed" ] ] } ] } db
                            |> Result.map (\next -> Dict.get "people" next.tables |> Maybe.map Dict.keys)
                            |> Expect.equal (Ok (Just [ Identity.uuid alice, Identity.uuid bob ]))
        , test "UUID indexed rows move between index buckets" <|
            \_ ->
                withDb <|
                    \db ->
                        Db.applyDelta { tableGroups = [ { tableName = "people", headers = [ "personKey", "name" ], rows = [ [ StringValue alice, StringValue "Renamed" ] ] } ] } db
                            |> Result.map (\next -> Dict.get ( "people", "name" ) next.indices |> Maybe.map (\index -> ( Db.Index.lookup "s:Alice" index, Db.Index.lookup "s:Renamed" index )))
                            |> Expect.equal (Ok (Just ( [], [ Identity.uuid alice ] )))
        , test "initially absent tables retain indexes for later UUID inserts" <|
            \_ ->
                Db.applyDelta { tableGroups = [ { tableName = "people", headers = [ "personKey", "name" ], rows = [ [ StringValue alice, StringValue "Alice" ] ] } ] } (Db.init schema)
                    |> Result.map (\next -> Dict.get ( "people", "name" ) next.indices |> Maybe.map (Db.Index.lookup "s:Alice"))
                    |> Expect.equal (Ok (Just [ Identity.uuid alice ]))
        , test "safe wire integer bounds and UUID spelling round trip without coercion" <|
            \_ ->
                Expect.all
                    [ \_ -> Identity.fromValue Schema.IntKey (IntValue 9007199254740991) |> Result.map Identity.toValue |> Expect.equal (Ok (IntValue 9007199254740991))
                    , \_ -> Identity.fromValue Schema.IntKey (IntValue -9007199254740991) |> Result.map Identity.toValue |> Expect.equal (Ok (IntValue -9007199254740991))
                    , \_ -> Identity.fromValue Schema.UuidKey (StringValue "ABCDEF00-0000-0000-0000-000000000000") |> Result.map Identity.toValue |> Expect.equal (Ok (StringValue "ABCDEF00-0000-0000-0000-000000000000"))
                    ]
                    ()
        , test "projection without identity publishes a full result, not a guessed id diff" <|
            \_ ->
                withDb <|
                    \db ->
                        let
                            delta =
                                noteDelta [ "number", "label" ] [ [ IntValue 2, StringValue "changed" ] ]

                            manager =
                                { subscriptions = Dict.singleton "q" (subscription db projection) }
                        in
                        case Db.applyDelta delta db of
                            Err error ->
                                Expect.fail error

                            Ok next ->
                                let
                                    ( updated, cmds ) =
                                        QueryManager.notifyTablesChanged schema next manager delta
                                in
                                Expect.equal
                                    ( Just 2, [ QueryManager.queryClientFull "q" 2 (Encode.object [ ( "note", Encode.list (\text -> Encode.object [ ( "id", Encode.string text ) ]) [ "changed", "Alice" ] ) ]) ] )
                                    ( Dict.get "q" updated.subscriptions |> Maybe.map .revision, cmds )
        , test "unchanged projections do not publish or advance revisions" <|
            \_ ->
                withDb <|
                    \db ->
                        let
                            delta =
                                noteDelta [ "number", "label", "updatedAt" ] [ [ IntValue 2, StringValue "Alice", IntValue 99 ] ]

                            manager =
                                { subscriptions = Dict.singleton "q" (subscription db projection) }
                        in
                        case Db.applyDelta delta db of
                            Err error ->
                                Expect.fail error

                            Ok next ->
                                Expect.equal ( manager, [] ) (QueryManager.notifyTablesChanged schema next manager delta)
        , test "nested table changes trigger aliased parent query re-execution" <|
            \_ ->
                withDb <|
                    \db ->
                        let
                            query =
                                Dict.singleton "person" (field [ ( "children", Query.SelectNested (Just "notes") (field []) ) ])
                        in
                        Expect.equal QueryManager.ReExecuteFull
                            (QueryManager.shouldReExecuteQuery schema db (subscription db query) (noteDelta [ "number" ] [ [ IntValue 2 ] ]))
        , test "same identity in distinct tables and database instances stays scoped" <|
            \_ ->
                let
                    metadata name =
                        { name = name, primaryKey = { name = "key", kind = Schema.IntKey }, links = Dict.empty, indices = [] }

                    scopedSchema =
                        { tables = Dict.fromList [ ( "a", metadata "a" ), ( "b", metadata "b" ) ], queryFieldToTable = Dict.empty }

                    delta =
                        { tableGroups = List.map (\name -> { tableName = name, headers = [ "key" ], rows = [ [ IntValue 2 ] ] }) [ "a", "b" ] }

                    untouched =
                        Db.init scopedSchema
                in
                Db.applyDelta delta untouched
                    |> Result.map (\db -> ( Dict.size db.tables, Dict.size untouched.tables, Dict.values db.tables |> List.map Dict.keys ))
                    |> Expect.equal (Ok ( 2, 0, [ [ Identity.int 2 ], [ Identity.int 2 ] ] ))
        , test "cursor wire values are raw UUIDs and numerically greatest integers on timestamp ties" <|
            \_ ->
                withDb <|
                    \db ->
                        let
                            server =
                                { baseUrl = "", catchupPath = "", databaseId = Nothing, headers = [], credentials = "same-origin", withCredentials = False }

                            result =
                                Catchup.update (Catchup.InitialDataLoaded Dict.empty Nothing) (Catchup.init server) db
                        in
                        Expect.equal ( Just (StringValue bob), Just (IntValue 10) )
                            ( Dict.get "people" result.model.cursor |> Maybe.andThen .lastSeenPrimaryKey
                            , Dict.get "notes" result.model.cursor |> Maybe.andThen .lastSeenPrimaryKey
                            )
        , describe "invalid batches are observable and atomic"
            (List.map
                (\( label, delta ) ->
                    test label <|
                        \_ ->
                            withDb <|
                                \db ->
                                    case Db.applyDelta delta db of
                                        Ok _ ->
                                            Expect.fail "Invalid delta was accepted"

                                        Err error ->
                                            Expect.equal ( db, Data.Error.sendError error ) (Db.update (Db.DeltaReceived delta) db)
                )
                [ ( "valid prefix followed by invalid integer", noteDelta [ "number" ] [ [ IntValue 2 ], [ StringValue "2" ] ] )
                , ( "missing non-id key", noteDelta [ "id" ] [ [ IntValue 2 ] ] )
                , ( "null key", noteDelta [ "number" ] [ [ NullValue ] ] )
                , ( "unsafe positive integer", noteDelta [ "number" ] [ [ IntValue 9007199254740992 ] ] )
                , ( "unsafe negative integer", noteDelta [ "number" ] [ [ IntValue -9007199254740992 ] ] )
                , ( "fractional key", noteDelta [ "number" ] [ [ FloatValue 1.5 ] ] )
                , ( "duplicate key", noteDelta [ "number" ] [ [ IntValue 2 ], [ IntValue 2 ] ] )
                , ( "truncated row", noteDelta [ "number", "label" ] [ [ IntValue 2 ] ] )
                , ( "extra column", noteDelta [ "number" ] [ [ IntValue 2, NullValue ] ] )
                , ( "duplicate header", noteDelta [ "number", "number" ] [ [ IntValue 2, IntValue 10 ] ] )
                , ( "invalid UUID", { tableGroups = [ { tableName = "people", headers = [ "personKey" ], rows = [ [ StringValue "not-a-uuid" ] ] } ] } )
                , ( "unknown table", { tableGroups = [ { tableName = "unknown", headers = [ "id" ], rows = [ [ IntValue 1 ] ] } ] } )
                ]
            )
        , test "initial data rejects invalid rows instead of installing a valid prefix" <|
            \_ ->
                Db.fromInitialData schema { initial | tables = Dict.insert "notes" [ Dict.singleton "number" (IntValue 1), Dict.singleton "id" (IntValue 2) ] initial.tables }
                    |> Expect.err
        , test "initial data rejects duplicate keys" <|
            \_ ->
                Db.fromInitialData schema { initial | tables = Dict.singleton "notes" (List.repeat 2 (Dict.singleton "number" (IntValue 1))) }
                    |> Expect.err
        , test "delta decoding rejects mismatched row widths" <|
            \_ ->
                Decode.decodeString Delta.decodeDelta """[{"table_name":"notes","headers":["number"],"rows":[[1,2]]}]""" |> Expect.err
        , test "metadata requires explicit identity and rejects unsupported kinds" <|
            \_ ->
                Expect.all
                    [ \_ -> Decode.decodeString Schema.decodeTableMetadata """{"name":"notes","links":{},"indices":[]}""" |> Expect.err
                    , \_ -> Decode.decodeString Schema.decodeTableMetadata """{"name":"notes","links":{},"indices":[],"primaryKey":{"name":"key","kind":"string"}}""" |> Expect.err
                    , \_ -> Decode.decodeString Schema.decodeTableMetadata """{"name":"notes","links":{},"indices":[],"primaryKey":{"name":"","kind":"int"}}""" |> Expect.err
                    , \_ -> Decode.decodeString Schema.decodeTableMetadata """{"name":"notes","links":{},"indices":[],"primaryKey":{"name":"key","kind":"uuid"}}""" |> Result.map .primaryKey |> Expect.equal (Ok { name = "key", kind = Schema.UuidKey })
                    ]
                    ()
        ]
