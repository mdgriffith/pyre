module UuidIdentityTest exposing (suite)

import Data.RowId
import Data.Value as Value
import Db
import Db.Index
import Dict
import Expect
import Test exposing (Test, describe, test)
import Uuid


suite : Test
suite =
    describe "Synced UUID identity"
        [ test "accepts imported UUID versions but rejects integer and malformed keys" <|
            \_ ->
                Expect.equal
                    [ Just (Uuid.id 1), Just "550e8400-e29b-41d4-a716-446655440000", Nothing, Nothing, Nothing ]
                    (List.map Data.RowId.fromValue
                        [ Uuid.value 1
                        , Value.StringValue "550e8400-e29b-41d4-a716-446655440000"
                        , Value.IntValue 1
                        , Value.StringValue "1"
                        , Value.StringValue "00000000-0000-7000-8000-00000000000z"
                        ]
                    )
        , test "delta headers locate UUID keys and index updates keep distinct rows" <|
            \_ ->
                let
                    initial =
                        { tables = Dict.empty
                        , indices = Dict.singleton ( "notes", "owner" ) Db.Index.empty
                        }

                    apply rows db =
                        Db.update
                            (Db.LocalDeltaReceived
                                { tableGroups =
                                    [ { tableName = "notes"
                                      , headers = [ "owner", "id" ]
                                      , rows = rows
                                      }
                                    ]
                                }
                            )
                            db
                            |> Tuple.first

                    result =
                        initial
                            |> apply [ [ Uuid.value 10, Uuid.value 1 ], [ Uuid.value 10, Uuid.value 2 ] ]
                            |> apply [ [ Uuid.value 20, Uuid.value 1 ] ]

                    indexed owner =
                        Dict.get ( "notes", "owner" ) result.indices
                            |> Maybe.map (Db.Index.lookup (Uuid.id owner))
                in
                Expect.equal
                    ( Just [ Uuid.id 1, Uuid.id 2 ], Just [ Uuid.id 2 ], Just [ Uuid.id 1 ] )
                    ( Dict.get "notes" result.tables |> Maybe.map Dict.keys, indexed 10, indexed 20 )
        ]
