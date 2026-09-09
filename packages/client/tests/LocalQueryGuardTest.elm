module LocalQueryGuardTest exposing (suite)

import Data.Error
import Data.LiveSync
import Data.QueryManager as QueryManager
import Db.Query
import Dict
import Expect
import Json.Decode as Decode
import Json.Encode as Encode
import Main
import Test exposing (Test, describe, test)


sessionError : String
sessionError =
    "Local queries cannot reference Session; use explicit inputs or execute on the server."


unsupportedShapes : List ( String, String )
unsupportedShapes =
    [ ( "generated marker", "{\"$error\":\"" ++ sessionError ++ "\"}" )
    , ( "Session predicate key", """{"users":{"id":true,"@where":{"Session.userId":{"$eq":1}}}}""" )
    , ( "Session operand", """{"users":{"id":true,"@where":{"id":{"$eq":{"$session":"userId"}}}}}""" )
    , ( "nested relationship", """{"users":{"id":true,"posts":{"id":true,"@where":{"Session.userId":1}}}}""" )
    , ( "boolean clauses", """{"users":{"@where":{"$or":[{"id":1},{"$and":[{"Session.userId":2}]}]}}}""" )
    , ( "variant operand", """{"users":{"@where":{"status":{"$eq":{"type":"Owned","owner":{"$session":"userId"}}}}}}""" )
    , ( "selection shortcut", """{"users":{"posts":{"@select":true,"@where":{"id":{"$session":"userId"}}}}}""" )
    , ( "array operand", """{"users":{"@where":{"id":{"$in":[1,{"nested":{"$session":"userId"}}]}}}}""" )
    ]


expectSessionError : Result Decode.Error a -> Expect.Expectation
expectSessionError result =
    case result of
        Err error ->
            Expect.equal True (String.contains sessionError (Decode.errorToString error))

        Ok _ ->
            Expect.fail "Unsupported local query was accepted"


suite : Test
suite =
    describe "Local Session query guard"
        [ describe "common query decoder"
            (List.map
                (\( label, shape ) ->
                    test label (\_ -> Decode.decodeString Db.Query.decodeQuery shape |> expectSessionError)
                )
                unsupportedShapes
            )
        , describe "port requests propagate failures"
            (List.concatMap
                (\requestType ->
                    List.map
                        (\( label, shape ) ->
                            test (requestType ++ " " ++ label)
                                (\_ ->
                                    Decode.decodeString QueryManager.decodeQueryClientIncoming
                                        ("{\"type\":\"" ++ requestType ++ "\",\"queryId\":\"existing\",\"queryInput\":{\"id\":99},\"querySource\":" ++ shape ++ "}")
                                        |> expectSessionError
                                )
                        )
                        unsupportedShapes
                )
                [ "register", "update-input" ]
            )
        , test "ordinary Session strings remain valid"
            (\_ ->
                Decode.decodeString Db.Query.decodeQuery
                    """{"users":{"id":true,"@where":{"name":{"$eq":"Session.userId"},"tags":{"$in":["Session.x","$session"]}}}}"""
                    |> Expect.ok
            )
        , describe "ordinary JSON operands remain valid"
            (List.concatMap
                (\( label, operand ) ->
                    List.map
                        (\requestType ->
                            test (requestType ++ " " ++ label)
                                (\_ ->
                                    Decode.decodeString QueryManager.decodeQueryClientIncoming
                                        ("{\"type\":\"" ++ requestType ++ "\",\"queryId\":\"existing\",\"queryInput\":{},\"querySource\":{\"users\":{\"@where\":{\"payload\":{\"$eq\":" ++ operand ++ "}}}}}")
                                        |> Expect.ok
                                )
                        )
                        [ "register", "update-input" ]
                )
                [ ( "non-singleton $session", """{"$session":"data","other":1}""" )
                , ( "Session key in literal", """{"Session.userId":42}""" )
                , ( "non-string nested $session", """{"nested":{"$session":false}}""" )
                , ( "literal predicate-like keys", """{"@where":{"Session.userId":42},"$or":[{"Session.userId":42}]}""" )
                , ( "array of JSON literals", """[{"Session.userId":42},{"$session":null}]""" )
                ]
            )
        , describe "invalid requests preserve the registered model and emit only an error"
            (List.concatMap
                (\requestType ->
                    List.map
                        (\( label, shape ) ->
                            test (requestType ++ " " ++ label)
                                (\_ ->
                                    let
                                        initialModel =
                                            Main.init
                                                { schema = { tables = Dict.empty, queryFieldToTable = Dict.empty }
                                                , server = { baseUrl = "", catchupPath = "", databaseId = Nothing, headers = [], credentials = "same-origin", withCredentials = False }
                                                , liveSync = { transport = Data.LiveSync.Sse }
                                                , sync = { autoStart = False }
                                                }
                                                |> Tuple.first

                                        registeredModel =
                                            Decode.decodeString QueryManager.decodeQueryClientIncoming
                                                """{"type":"register","queryId":"existing","querySource":{"users":{"id":true}},"queryInput":{"id":1}}"""
                                                |> Main.queryClientMessage
                                                |> (\msg -> Main.update msg initialModel)
                                                |> Tuple.first

                                        decoded =
                                            Decode.decodeString QueryManager.decodeQueryClientIncoming
                                                ("{\"type\":\"" ++ requestType ++ "\",\"queryId\":\"existing\",\"queryInput\":{\"id\":99},\"querySource\":" ++ shape ++ "}")

                                        ( after, cmd ) =
                                            Main.update (Main.queryClientMessage decoded) registeredModel
                                    in
                                    case decoded of
                                        Ok _ ->
                                            Expect.fail "Invalid request decoded successfully"

                                        Err error ->
                                            Expect.all
                                                [ \_ -> Expect.equal registeredModel after
                                                , \_ ->
                                                    Expect.equal
                                                        (Just (Encode.object [ ( "id", Encode.int 1 ) ]))
                                                        (Dict.get "existing" after.queryManager.subscriptions |> Maybe.map .input)
                                                , \_ ->
                                                    Expect.equal
                                                        (Data.Error.sendError ("Failed to decode QueryClient message: " ++ Decode.errorToString error))
                                                        cmd
                                                ]
                                                ()
                                )
                        )
                        unsupportedShapes
                )
                [ "register", "update-input" ]
            )
        , test "omitted update shape remains supported"
            (\_ ->
                case Decode.decodeString QueryManager.decodeQueryClientIncoming """{"type":"update-input","queryId":"existing","queryInput":{"id":99}}""" of
                    Ok (QueryManager.QCUpdateInput "existing" Nothing _) ->
                        Expect.pass

                    _ ->
                        Expect.fail "An omitted querySource should decode as Nothing"
            )
        , test "valid replacement shape remains supported"
            (\_ ->
                case Decode.decodeString QueryManager.decodeQueryClientIncoming """{"type":"update-input","queryId":"existing","querySource":{"users":{"id":true}},"queryInput":{"id":99}}""" of
                    Ok (QueryManager.QCUpdateInput "existing" (Just _) _) ->
                        Expect.pass

                    _ ->
                        Expect.fail "A valid querySource should decode as Just"
            )
        , test "present malformed update shape is not treated as omitted"
            (\_ ->
                Decode.decodeString QueryManager.decodeQueryClientIncoming """{"type":"update-input","queryId":"existing","querySource":null,"queryInput":{"id":99}}"""
                    |> Expect.err
            )
        ]
