module CustomKeyTest exposing (tests)

import Data.QueryManager
import Data.Schema
import Data.Value exposing (Value(..))
import Db
import Db.Index
import Db.Query
import Dict
import Expect
import Set
import Test exposing (Test, test)
import Uuid


tests : Test
tests =
    test "custom primary keys drive indexed relations, projected tracking and removal" <|
        \_ ->
            let
                schema =
                    { tables =
                        Dict.fromList
                            [ ( "parents"
                              , { name = "parents"
                                , indices = [ { field = "key", primary = True, unique = True } ]
                                , links = Dict.singleton "children" { type_ = Data.Schema.OneToMany, from = "key", to = { table = "children", column = "parent" } }
                                }
                              )
                            , ( "children", { name = "children", indices = [ { field = "childKey", primary = True, unique = True } ], links = Dict.empty } )
                            ]
                    , queryFieldToTable = Dict.singleton "parents" "parents"
                    }

                initial =
                    { tableGroups =
                        [ { tableName = "parents", headers = [ "key", "id" ], rows = [ [ Uuid.value 1, StringValue "ordinary" ] ] }
                        , { tableName = "children", headers = [ "childKey", "parent", "id" ], rows = [ [ Uuid.value 2, Uuid.value 1, StringValue "ordinary" ] ] }
                        ]
                    }

                db =
                    Db.update (Db.LocalDeltaReceived initial) (Db.initWithSchema schema) |> Tuple.first

                childQuery =
                    { selections = Dict.empty, where_ = Nothing, sort = Nothing, limit = Nothing }

                query =
                    Dict.singleton "parents"
                        { childQuery
                            | selections = Dict.fromList [ ( "identity", Db.Query.SelectField (Just "key") ), ( "children", Db.Query.SelectNested Nothing childQuery ) ]
                            , where_ = Just (Dict.singleton "key" (Db.Query.FilterValueSimple (Uuid.value 1)))
                        }

                result =
                    Db.executeQueryWithTracking schema db query

                removal =
                    { tableGroups = [ { tableName = "children", headers = [ "childKey", "_pyre_removed" ], rows = [ [ Uuid.value 2, BoolValue True ] ] } ] }

                removed =
                    Db.update (Db.LocalDeltaReceived removal) db |> Tuple.first
            in
            Expect.equal
                { tracked = Just (Set.singleton (Uuid.id 1)), indexed = Just [ Uuid.id 2 ], changed = Just (Set.singleton (Uuid.id 2)), removed = Just [], related = Just (ArrayValue [ ObjectValue (Dict.fromList [ ( "childKey", Uuid.value 2 ), ( "parent", Uuid.value 1 ), ( "id", StringValue "ordinary" ) ]) ]) }
                { tracked = Dict.get "parents" result.rowIds
                , indexed = Dict.get ( "children", "parent" ) db.indices |> Maybe.map (Db.Index.lookup (Uuid.id 1))
                , changed = Dict.get "children" (Data.QueryManager.extractChangedRowIds schema removal)
                , removed = Dict.get ( "children", "parent" ) removed.indices |> Maybe.map (Db.Index.lookup (Uuid.id 1))
                , related = Dict.get "parents" result.results |> Maybe.andThen List.head |> Maybe.andThen (Dict.get "children")
                }
