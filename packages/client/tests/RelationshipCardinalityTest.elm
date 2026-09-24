module RelationshipCardinalityTest exposing (suite)

import Data.Schema
import Data.Value exposing (Value)
import Db
import Db.Query
import Dict exposing (Dict)
import Expect
import Test exposing (Test, describe, test)
import Uuid


suite : Test
suite =
    describe "Relationship cardinality"
        [ test "one-to-many returns [] when related table exists but has no rows" <|
            \_ ->
                let
                    result =
                        Db.executeQuery schema dbWithEmptyRelatedTable listGamesQuery
                in
                Expect.equal expectedResult result
        , test "one-to-many should return [] even when related table is missing" <|
            \_ ->
                let
                    result =
                        Db.executeQuery schema dbWithoutRelatedTable listGamesQuery
                in
                Expect.equal expectedResult result
        , test "syncing only parent rows still returns [] for one-to-many" <|
            \_ ->
                let
                    gamesOnlyDelta =
                        { tableGroups =
                            [ { tableName = "games"
                              , headers = [ "id", "name" ]
                              , rows =
                                    [ [ Uuid.value 1
                                      , Data.Value.StringValue "The Broken Tower"
                                      ]
                                    ]
                              }
                            ]
                        }

                    dbAfterDelta =
                        Db.update (Db.DeltaReceived gamesOnlyDelta) Db.init
                            |> Tuple.first

                    result =
                        Db.executeQuery schema dbAfterDelta listGamesQuery
                in
                Expect.equal
                    (Dict.fromList
                        [ ( "game"
                          , [ Dict.fromList
                                [ ( "id", Uuid.value 1 )
                                , ( "name", Data.Value.StringValue "The Broken Tower" )
                                , ( "gameMembers", Data.Value.ArrayValue [] )
                                ]
                            ]
                          )
                        ]
                    )
                    result
        , test "aliased one-to-many resolves by source link and projects alias" <|
            \_ ->
                let
                    result =
                        Db.executeQuery schema dbWithRelatedRows aliasedListGamesQuery
                in
                Expect.equal
                    (Dict.fromList
                        [ ( "game"
                          , [ Dict.fromList
                                [ ( "id", Uuid.value 1 )
                                , ( "members"
                                  , Data.Value.ArrayValue
                                        [ Data.Value.ObjectValue
                                            (Dict.fromList
                                                [ ( "id", Uuid.value 10 )
                                                , ( "userId", Uuid.value 20 )
                                                ]
                                            )
                                        ]
                                  )
                                ]
                            ]
                          )
                        ]
                    )
                    result
        ]


schema : Data.Schema.SchemaMetadata
schema =
    { tables =
        Dict.fromList
            [ ( "games"
              , { name = "games"
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
                , links = Dict.empty
                , indices = []
                }
              )
            ]
    , queryFieldToTable = Dict.fromList [ ( "game", "games" ) ]
    }


listGamesQuery : Db.Query.Query
listGamesQuery =
    Dict.fromList
        [ ( "game"
          , { selections =
                Dict.fromList
                    [ ( "id", Db.Query.SelectField Nothing )
                    , ( "name", Db.Query.SelectField Nothing )
                    , ( "gameMembers", Db.Query.SelectNested Nothing gameMembersFieldQuery )
                    ]
            , where_ = Nothing
            , sort = Nothing
            , limit = Nothing
            }
          )
        ]


gameMembersFieldQuery : Db.Query.FieldQuery
gameMembersFieldQuery =
    { selections =
        Dict.fromList
            [ ( "id", Db.Query.SelectField Nothing )
            , ( "userId", Db.Query.SelectField Nothing )
            ]
    , where_ = Nothing
    , sort = Nothing
    , limit = Nothing
    }


aliasedListGamesQuery : Db.Query.Query
aliasedListGamesQuery =
    Dict.fromList
        [ ( "game"
          , { selections =
                Dict.fromList
                    [ ( "id", Db.Query.SelectField Nothing )
                    , ( "members", Db.Query.SelectNested (Just "gameMembers") gameMembersFieldQuery )
                    ]
            , where_ = Nothing
            , sort = Nothing
            , limit = Nothing
            }
          )
        ]


dbWithEmptyRelatedTable : Db.Db
dbWithEmptyRelatedTable =
    { tables =
        Dict.fromList
            [ ( "games"
              , Dict.fromList
                    [ ( Uuid.id 1
                      , Dict.fromList
                            [ ( "id", Uuid.value 1 )
                            , ( "name", Data.Value.StringValue "The Broken Tower" )
                            ]
                      )
                    ]
              )
            , ( "game_members", Dict.empty )
            ]
    , indices = Dict.empty
    , primaryKeys = Dict.empty
    }


dbWithoutRelatedTable : Db.Db
dbWithoutRelatedTable =
    { tables =
        Dict.fromList
            [ ( "games"
              , Dict.fromList
                    [ ( Uuid.id 1
                      , Dict.fromList
                            [ ( "id", Uuid.value 1 )
                            , ( "name", Data.Value.StringValue "The Broken Tower" )
                            ]
                      )
                    ]
              )
            ]
    , indices = Dict.empty
    , primaryKeys = Dict.empty
    }


dbWithRelatedRows : Db.Db
dbWithRelatedRows =
    { tables =
        Dict.fromList
            [ ( "games"
              , Dict.fromList
                    [ ( Uuid.id 1
                      , Dict.fromList
                            [ ( "id", Uuid.value 1 )
                            , ( "name", Data.Value.StringValue "The Broken Tower" )
                            ]
                      )
                    ]
              )
            , ( "game_members"
              , Dict.fromList
                    [ ( Uuid.id 10
                      , Dict.fromList
                            [ ( "id", Uuid.value 10 )
                            , ( "gameId", Uuid.value 1 )
                            , ( "userId", Uuid.value 20 )
                            ]
                      )
                    ]
              )
            ]
    , indices = Dict.empty
    , primaryKeys = Dict.empty
    }


expectedResult : Dict String (List (Dict String Value))
expectedResult =
    Dict.fromList
        [ ( "game"
          , [ Dict.fromList
                [ ( "id", Uuid.value 1 )
                , ( "name", Data.Value.StringValue "The Broken Tower" )
                , ( "gameMembers", Data.Value.ArrayValue [] )
                ]
            ]
          )
        ]
