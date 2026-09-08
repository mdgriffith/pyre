module DeletionTest exposing (suite)

import Data.Schema
import Data.Value exposing (Value(..))
import Db
import Db.Index
import Dict
import Expect
import Test exposing (Test, describe, test)


suite : Test
suite =
    describe "Durable deletion application"
        [ test "deleting a cached row removes every index membership" <|
            \_ ->
                let
                    db =
                        initial (IntValue 1)

                    deleted =
                        Db.update (Db.LocalDeltaReceived { tableGroups = [ deletion (IntValue 1) ] }) db |> Tuple.first
                in
                Expect.equal ( Just 0, Just [] )
                    ( Dict.get "notes" deleted.tables |> Maybe.map Dict.size
                    , Dict.get ( "notes", "body" ) deleted.indices |> Maybe.map (Db.Index.lookup "old")
                    )
        , test "unknown text deletion and repeated deletion are no-ops" <|
            \_ ->
                let
                    db =
                        initial (StringValue "007")

                    unknown =
                        Db.update (Db.LocalDeltaReceived { tableGroups = [ deletion (StringValue "missing") ] }) db |> Tuple.first

                    deleted =
                        Db.update (Db.LocalDeltaReceived { tableGroups = [ deletion (StringValue "007"), deletion (StringValue "007") ] }) unknown |> Tuple.first
                in
                Expect.equal ( True, Just 0, Just [] )
                    ( db == unknown
                    , Dict.get "notes" deleted.tables |> Maybe.map Dict.size
                    , Dict.get ( "notes", "id" ) deleted.indices |> Maybe.map (Db.Index.lookup "007")
                    )
        , test "delete then reinsert preserves the new row and replaces index keys" <|
            \_ ->
                let
                    updated =
                        Db.update
                            (Db.LocalDeltaReceived
                                { tableGroups =
                                    [ deletion (IntValue 1)
                                    , { tableName = "notes", headers = [ "id", "body" ], rows = [ [ IntValue 1, StringValue "new" ] ] }
                                    ]
                                }
                            )
                            (initial (IntValue 1))
                            |> Tuple.first
                in
                Expect.equal ( Just [], Just [ 1 ] )
                    ( Dict.get ( "notes", "body" ) updated.indices |> Maybe.map (Db.Index.lookup "old")
                    , Dict.get ( "notes", "body" ) updated.indices |> Maybe.map (Db.Index.lookup "new")
                    )
        , test "an unknown integer deletion cannot remove a text row's internal key" <|
            \_ ->
                let
                    db =
                        initial (StringValue "007")

                    deleted =
                        Db.update (Db.LocalDeltaReceived { tableGroups = [ deletion (IntValue -1) ] }) db |> Tuple.first
                in
                Expect.equal db deleted
        ]


deletion key =
    { tableName = "notes", headers = [ "$delete" ], rows = [ [ key ] ] }


initial key =
    Db.fromInitialData
        { tables =
            Dict.singleton "notes"
                { name = "notes"
                , links = Dict.empty
                , indices =
                    [ { field = "id", unique = True, primary = True }
                    , { field = "body", unique = False, primary = False }
                    ]
                }
        , queryFieldToTable = Dict.singleton "notes" "notes"
        }
        { tables = Dict.singleton "notes" [ Dict.fromList [ ( "id", key ), ( "body", StringValue "old" ) ] ]
        , cursor = Dict.empty
        , lastAppliedServerRevision = Nothing
        , databaseEpoch = Just "epoch"
        }
