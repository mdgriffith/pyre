module LocalEditsTest exposing (suite)

import Data.Identity as Identity
import Data.LocalEdits as Edits exposing (State(..))
import Data.Schema as Schema
import Data.Value as Value
import Dict
import Expect
import Json.Decode as D
import Json.Encode as E
import Set
import Test exposing (Test, describe, test)


schema : Schema.SchemaMetadata
schema =
    { tables =
        Dict.singleton "users"
            { name = "users"
            , columns =
                Just
                    [ { name = "key", type_ = "Id.Uuid<Users>", nullable = False, codec = Just Schema.UuidCodec }
                    , { name = "name", type_ = "String", nullable = False, codec = Just Schema.StringCodec }
                    , { name = "note", type_ = "String", nullable = True, codec = Just (Schema.NullableCodec Schema.StringCodec) }
                    , { name = "day", type_ = "Date", nullable = False, codec = Just Schema.DateCodec }
                    , { name = "changedAt", type_ = "DateTime", nullable = False, codec = Just Schema.DateTimeCodec }
                    , { name = "enabled", type_ = "Bool", nullable = False, codec = Just Schema.BoolCodec }
                    , { name = "flags", type_ = "Json<List<Bool>>", nullable = False, codec = Just (Schema.ListCodec Schema.BoolCodec) }
                    , { name = "choice", type_ = "Choice", nullable = False, codec = Just (Schema.EnumCodec (Set.fromList [ "Open", "Closed" ])) }
                    , { name = "details"
                      , type_ = "Details"
                      , nullable = False
                      , codec =
                            Just
                                (Schema.NamedCodec "Details"
                                    (Schema.TaggedUnionCodec
                                        (Dict.fromList
                                            [ ( "Empty", Dict.empty )
                                            , ( "Group"
                                              , Dict.fromList
                                                    [ ( "members", Schema.ListCodec Schema.UuidCodec )
                                                    , ( "scores", Schema.DictCodec Schema.FloatCodec )
                                                    , ( "next", Schema.NullableCodec (Schema.ReferenceCodec "Details") )
                                                    ]
                                              )
                                            ]
                                        )
                                    )
                                )
                      }
                    , { name = "payload", type_ = "Json", nullable = False, codec = Just Schema.JsonCodec }
                    ]
            , links = Dict.empty
            , indices = [ { field = "name", unique = False, primary = False } ]
            , primaryKey = { name = "key", kind = Schema.UuidKey }
            }
    , queryFieldToTable = Dict.singleton "users" "users"
    }


uuid : String
uuid =
    "11111111-1111-4111-8111-111111111111"


fence : List ( String, E.Value )
fence =
    [ ( "databaseId", E.string "main" ), ( "instance", E.string "one" ), ( "authGeneration", E.int 0 ), ( "namespace", E.string "Main" ), ( "manifest", E.string "m1" ), ( "databaseEpoch", E.string "e1" ) ]


message : String -> List ( String, E.Value ) -> E.Value
message kind fields =
    E.object (( "type", E.string kind ) :: (fence ++ fields))


step : String -> List ( String, E.Value ) -> Edits.Model -> Edits.Model
step kind fields model =
    let
        next =
            Edits.receive (message kind fields) model |> Tuple.first
    in
    List.foldl
        (\pending current ->
            case pending.state of
                Preparing dispatchId ->
                    Edits.receive (message "prepared" [ ( "requestId", E.string pending.requestId ), ( "dispatchId", E.int dispatchId ) ]) current |> Tuple.first

                _ ->
                    current
        )
        next
        next.pending


typedRow : E.Value -> E.Value -> E.Value -> E.Value -> E.Value -> E.Value
typedRow key name note choice details =
    E.object
        [ ( "key", key )
        , ( "name", name )
        , ( "note", note )
        , ( "day", E.string "2026-09-15" )
        , ( "changedAt", E.int 1700000000 )
        , ( "enabled", E.bool True )
        , ( "flags", E.list E.bool [ True, False ] )
        , ( "choice", choice )
        , ( "details", details )
        , ( "payload", E.object [ ( "anything", E.list identity [ E.int 1, E.bool True ] ) ] )
        ]


replaceField : String -> E.Value -> E.Value -> E.Value
replaceField name value encoded =
    D.decodeValue (D.dict D.value) encoded
        |> Result.map (Dict.insert name value >> Dict.toList >> E.object)
        |> Result.withDefault encoded


row : String -> String -> E.Value
row name note =
    typedRow
        (E.string uuid)
        (E.string name)
        (E.string note)
        (E.object [ ( "_type", E.string "Open" ) ])
        (E.object
            [ ( "_type", E.string "Group" )
            , ( "members", E.list E.string [ uuid ] )
            , ( "scores", E.object [ ( "one", E.float 1.5 ) ] )
            , ( "next", E.object [ ( "_type", E.string "Empty" ) ] )
            ]
        )


replacement : Int -> List E.Value -> Edits.Model -> E.Value
replacement revision rows model =
    let
        ( requestId, target ) =
            Maybe.withDefault ( "missing", -1 ) model.catchup
    in
    message "replacement"
        [ ( "requestId", E.string requestId ), ( "target", E.int target ), ( "serverRevision", E.int revision ), ( "scope", E.string "database" ), ( "complete", E.bool True ), ( "tables", E.object [ ( "users", E.object [ ( "rows", E.list identity rows ) ] ) ] ) ]


install : Int -> List E.Value -> Edits.Model -> Edits.Model
install revision rows model =
    Edits.receive (replacement revision rows model) model |> Tuple.first


initial : List E.Value -> Edits.Model
initial rows =
    Edits.init schema
        |> step "configure" [ ( "minimumSafeRevision", E.int 0 ) ]
        |> install 0 rows


hint : Int -> E.Value
hint revision =
    E.object [ ( "kind", E.string "replaceRequired" ), ( "atLeast", E.int revision ), ( "invalidate", E.bool False ) ]


invalidatingHint : Int -> E.Value
invalidatingHint revision =
    E.object [ ( "kind", E.string "replaceRequired" ), ( "atLeast", E.int revision ), ( "invalidate", E.bool True ), ( "minimumSafeRevision", E.int revision ) ]


operation : String -> List ( String, E.Value ) -> E.Value
operation kind fields =
    E.object
        [ ( "operation", E.string (kind ++ "@hash") )
        , ( "input", E.object (( "key", E.string uuid ) :: fields) )
        , ( "prediction"
          , E.object
                [ ( "safe", E.bool True ), ( "kind", E.string kind ), ( "table", E.string "users" ), ( "id", E.string uuid ), ( "fields", E.object fields ), ( "writableFields", E.list E.string [ "name", "note" ] ), ( "materializedFields", E.list E.string [ "key", "name", "note" ] ) ]
          )
        ]


submit : String -> List E.Value -> Edits.Model -> Edits.Model
submit requestId operations =
    step "submit" [ ( "requestId", E.string requestId ), ( "operations", E.list identity operations ) ]


cancel : String -> Edits.Model -> Edits.Model
cancel requestId =
    step "cancel" [ ( "requestId", E.string requestId ) ]


accept : String -> Int -> Edits.Model -> Edits.Model
accept requestId revision =
    step "response"
        [ ( "requestId", E.string requestId )
        , ( "response"
          , E.object
                (fence
                    ++ [ ( "requestId", E.string requestId ), ( "status", E.string "accepted" ), ( "commitRevision", E.int revision ), ( "reconciliation", hint revision ), ( "results", E.list identity [ E.object [ ( "index", E.int 0 ), ( "operation", E.string "update@hash" ), ( "value", E.object [ ( "id", E.string uuid ) ] ) ] ] ) ]
                )
          )
        ]


acceptInvalidating : String -> Int -> Edits.Model -> Edits.Model
acceptInvalidating requestId revision =
    step "response"
        [ ( "requestId", E.string requestId )
        , ( "response"
          , E.object
                (fence
                    ++ [ ( "requestId", E.string requestId ), ( "status", E.string "accepted" ), ( "commitRevision", E.int revision ), ( "reconciliation", invalidatingHint revision ), ( "results", E.list identity [ E.object [ ( "index", E.int 0 ), ( "operation", E.string "update@hash" ), ( "value", E.object [ ( "id", E.string uuid ) ] ) ] ] ) ]
                )
          )
        ]


field : String -> Edits.Model -> Maybe Value.Value
field name model =
    (Edits.visible model).tables
        |> Dict.get "users"
        |> Maybe.andThen (Dict.get (Identity.uuid uuid))
        |> Maybe.andThen (Dict.get name)


permutations : List a -> List (List a)
permutations items =
    case items of
        [] ->
            [ [] ]

        _ ->
            List.indexedMap (\index item -> permutations (List.take index items ++ List.drop (index + 1) items) |> List.map ((::) item)) items |> List.concat


suite : Test
suite =
    describe "Production local edit reducer"
        [ describe "all cancellation orders preserve the last surviving field intent"
            (permutations [ "a", "b", "c" ]
                |> List.map
                    (\order ->
                        test (String.join "/" order)
                            (\_ ->
                                let
                                    queued =
                                        initial [ row "base" "original" ]
                                            |> step "connection" [ ( "connected", E.bool False ) ]
                                            |> submit "a" [ operation "update" [ ( "name", E.string "a" ) ] ]
                                            |> submit "b" [ operation "update" [ ( "name", E.string "b" ) ] ]
                                            |> submit "c" [ operation "update" [ ( "note", E.string "c" ) ] ]

                                    ( _, observations ) =
                                        List.foldl
                                            (\id ( model, checks ) ->
                                                let
                                                    next =
                                                        cancel id model

                                                    remains key =
                                                        List.any (\p -> p.requestId == key) next.pending

                                                    expectedName =
                                                        if remains "b" then
                                                            "b"

                                                        else if remains "a" then
                                                            "a"

                                                        else
                                                            "base"

                                                    expectedNote =
                                                        if remains "c" then
                                                            "c"

                                                        else
                                                            "original"
                                                in
                                                ( next, (field "name" next == Just (Value.StringValue expectedName) && field "note" next == Just (Value.StringValue expectedNote)) :: checks )
                                            )
                                            ( queued, [] )
                                            order
                                in
                                Expect.equal [ True, True, True ] observations
                            )
                    )
            )
        , describe "create/update/delete cancellation permutations never resurrect an absent create"
            (permutations [ "create", "update", "delete" ]
                |> List.map
                    (\order ->
                        test (String.join "/" order)
                            (\_ ->
                                let
                                    queued =
                                        initial []
                                            |> step "connection" [ ( "connected", E.bool False ) ]
                                            |> submit "create" [ operation "create" [ ( "key", E.string uuid ), ( "name", E.string "created" ), ( "note", E.null ) ] ]
                                            |> submit "update" [ operation "update" [ ( "name", E.string "updated" ) ] ]
                                            |> submit "delete" [ operation "delete" [] ]

                                    ( _, checks ) =
                                        List.foldl
                                            (\id ( model, observations ) ->
                                                let
                                                    next =
                                                        cancel id model

                                                    remains key =
                                                        List.any (\p -> p.requestId == key) next.pending

                                                    expected =
                                                        if not (remains "create") || remains "delete" then
                                                            Nothing

                                                        else if remains "update" then
                                                            Just (Value.StringValue "updated")

                                                        else
                                                            Just (Value.StringValue "created")
                                                in
                                                ( next, (field "name" next == expected) :: observations )
                                            )
                                            ( queued, [] )
                                            order
                                in
                                Expect.equal [ True, True, True ] checks
                            )
                    )
            )
        , test "replacement-before-acceptance quarantines without inventing an unknown outcome"
            (\_ ->
                let
                    sent =
                        initial [ row "base" "original" ]
                            |> submit "a" [ operation "update" [ ( "name", E.string "local" ) ] ]
                            |> step "syncRequired" [ ( "reconciliation", hint 1 ) ]

                    replaced =
                        install 1 [ row "server" "corrected" ] sent
                in
                Expect.equal
                    ( [ ( Sent, True ) ], Just (Value.StringValue "server") )
                    ( List.map (\p -> ( p.state, p.quarantined )) replaced.pending, field "name" replaced )
            )
        , test "late acceptance above coverage restores only setters on a proven precommit base"
            (\_ ->
                let
                    accepted =
                        initial [ row "base" "original" ]
                            |> submit "a" [ operation "update" [ ( "name", E.string "local" ) ] ]
                            |> step "syncRequired" [ ( "reconciliation", hint 1 ) ]
                            |> install 1 [ row "server" "corrected" ]
                            |> accept "a" 2
                in
                Expect.equal
                    ( [ Just (Value.StringValue "local"), Just (Value.StringValue "corrected") ], [ False ], ( 1, 2 ) )
                    ( [ field "name" accepted, field "note" accepted ], List.map .quarantined accepted.pending, ( accepted.coveredRevision, accepted.requiredRevision ) )
            )
        , test "late invalidating acceptance already covered by its safety minimum stays installed"
            (\_ ->
                let
                    accepted =
                        initial [ row "base" "original" ]
                            |> submit "a" [ operation "update" [ ( "name", E.string "local" ) ] ]
                            |> step "syncRequired" [ ( "reconciliation", hint 1 ) ]
                            |> install 1 [ row "server" "current" ]
                            |> acceptInvalidating "a" 1
                in
                Expect.equal
                    ( ( False, Nothing ), ( 1, Just (Value.StringValue "server") ), [] )
                    ( ( accepted.invalid, accepted.catchup ), ( accepted.coveredRevision, field "name" accepted ), accepted.pending )
            )
        , test "malformed complete replacement rows do not clear invalidation or advance coverage"
            (\_ ->
                let
                    candidates =
                        [ E.object [ ( "key", E.string uuid ), ( "name", E.string "missing nullable field" ) ]
                        , E.object [ ( "key", E.string uuid ), ( "name", E.string "extra" ), ( "note", E.null ), ( "secret", E.string "no" ) ]
                        , E.object [ ( "key", E.string uuid ), ( "name", E.int 1 ), ( "note", E.null ) ]
                        , E.object [ ( "key", E.string uuid ), ( "name", E.null ), ( "note", E.null ) ]
                        , typedRow (E.string "not-a-uuid") (E.string "bad key") E.null (E.object [ ( "_type", E.string "Open" ) ]) (E.object [ ( "_type", E.string "Empty" ) ])
                        , typedRow (E.string uuid) (E.string "scalar enum") E.null (E.string "Open") (E.object [ ( "_type", E.string "Empty" ) ])
                        , typedRow (E.string uuid) (E.string "bad enum") E.null (E.object [ ( "_type", E.string "Unknown" ) ]) (E.object [ ( "_type", E.string "Empty" ) ])
                        , typedRow (E.string uuid) (E.string "bad list") E.null (E.object [ ( "_type", E.string "Open" ) ]) (E.object [ ( "_type", E.string "Group" ), ( "members", E.list E.string [ "bad" ] ), ( "scores", E.object [] ), ( "next", E.null ) ])
                        , typedRow (E.string uuid) (E.string "bad dict") E.null (E.object [ ( "_type", E.string "Open" ) ]) (E.object [ ( "_type", E.string "Group" ), ( "members", E.list E.string [] ), ( "scores", E.object [ ( "bad", E.string "float" ) ] ), ( "next", E.null ) ])
                        , typedRow (E.string uuid) (E.string "missing union field") E.null (E.object [ ( "_type", E.string "Open" ) ]) (E.object [ ( "_type", E.string "Group" ), ( "members", E.list E.string [] ), ( "scores", E.object [] ) ])
                        , typedRow (E.string uuid) (E.string "extra union field") E.null (E.object [ ( "_type", E.string "Open" ) ]) (E.object [ ( "_type", E.string "Empty" ), ( "extra", E.bool True ) ])
                        , typedRow (E.string uuid) (E.string "bad bool") E.null (E.object [ ( "_type", E.string "Open" ) ]) (E.object [ ( "_type", E.string "Empty" ) ])
                            |> replaceField "enabled" (E.int 2)
                        , typedRow (E.string uuid) (E.string "storage bool") E.null (E.object [ ( "_type", E.string "Open" ) ]) (E.object [ ( "_type", E.string "Empty" ) ])
                            |> replaceField "enabled" (E.int 1)
                        , typedRow (E.string uuid) (E.string "typed JSON bool") E.null (E.object [ ( "_type", E.string "Open" ) ]) (E.object [ ( "_type", E.string "Empty" ) ])
                            |> replaceField "flags" (E.list E.int [ 1 ])
                        , typedRow (E.string uuid) (E.string "string datetime") E.null (E.object [ ( "_type", E.string "Open" ) ]) (E.object [ ( "_type", E.string "Empty" ) ])
                            |> replaceField "changedAt" (E.string "1700000000")
                        , typedRow (E.string uuid) (E.string "bad datetime") E.null (E.object [ ( "_type", E.string "Open" ) ]) (E.object [ ( "_type", E.string "Empty" ) ])
                            |> replaceField "changedAt" (E.int 8640000000001)
                        ]

                    check candidate =
                        let
                            before =
                                initial [ row "base" "original" ]
                                    |> step "syncRequired" [ ( "reconciliation", invalidatingHint 1 ) ]

                            after =
                                install 1 [ candidate ] before
                        in
                        ( ( after.invalid, after.coveredRevision ), ( after.catchupFailed, field "name" after ) )
                in
                candidates
                    |> List.map check
                    |> Expect.equal (List.repeat 16 ( ( True, 0 ), ( True, Nothing ) ))
            )
        , test "canonical replacement enum, datetime, and bool values are accepted"
            (\_ ->
                let
                    canonical =
                        row "server" "value"

                    installed =
                        initial [ row "base" "original" ]
                            |> step "syncRequired" [ ( "reconciliation", invalidatingHint 1 ) ]
                            |> install 1 [ canonical ]
                in
                Expect.equal
                    ( ( False, 1 ), ( False, Just (Value.ObjectValue (Dict.singleton "_type" (Value.StringValue "Open"))) ) )
                    ( ( installed.invalid, installed.coveredRevision ), ( installed.catchupFailed, field "choice" installed ) )
            )
        , test "nonempty replacements require generated column codec metadata"
            (\_ ->
                let
                    withoutColumns =
                        { schema | tables = Dict.map (\_ table -> { table | columns = Nothing }) schema.tables }

                    withoutCodecs =
                        { schema
                            | tables =
                                Dict.map
                                    (\_ table ->
                                        { table | columns = table.columns |> Maybe.map (List.map (\column -> { column | codec = Nothing })) }
                                    )
                                    schema.tables
                        }

                    attempt testSchema =
                        Edits.init testSchema
                            |> step "configure" [ ( "minimumSafeRevision", E.int 0 ) ]
                            |> install 0 [ row "server" "value" ]

                    summarize model =
                        ( model.invalid, model.coveredRevision, model.catchupFailed )
                in
                [ attempt withoutColumns, attempt withoutCodecs ]
                    |> List.map summarize
                    |> Expect.equal [ ( True, -1, True ), ( True, -1, True ) ]
            )
        , test "a missing replay target suppresses every earlier member atomically"
            (\_ ->
                let
                    missingDelete =
                        E.object [ ( "operation", E.string "delete@hash" ), ( "input", E.object [] ), ( "prediction", E.object [ ( "safe", E.bool True ), ( "kind", E.string "delete" ), ( "table", E.string "users" ), ( "id", E.string "22222222-2222-4222-8222-222222222222" ) ] ) ]

                    model =
                        initial [] |> submit "a" [ operation "create" [ ( "key", E.string uuid ), ( "name", E.string "created" ), ( "note", E.null ) ], missingDelete ]
                in
                Expect.equal ( Nothing, [ Sent ] ) ( field "name" model, List.map .state model.pending )
            )
        , test "empty batches confirm without reserving sequence, dispatch, or visible events"
            (\_ ->
                let
                    before =
                        initial []

                    ( after, events ) =
                        Edits.receive (message "submit" [ ( "requestId", E.string "empty" ), ( "operations", E.list identity [] ) ]) before
                in
                Expect.equal
                    ( ( 0, [] ), [ Ok "lifecycle" ], [ Ok "confirmed" ] )
                    ( ( after.sequence, List.map .requestId after.pending ), List.map (D.decodeValue (D.field "type" D.string)) events, List.map (D.decodeValue (D.field "state" D.string)) events )
            )
        , test "invalid atomic member rejects all intent before dispatch"
            (\_ ->
                let
                    before =
                        initial [ row "base" "original" ]

                    ( after, events ) =
                        Edits.receive (message "submit" [ ( "requestId", E.string "invalid" ), ( "operations", E.list identity [ operation "update" [ ( "name", E.string "hidden-prefix" ) ], operation "update" [] ] ) ]) before
                in
                Expect.equal
                    ( Just (Value.StringValue "base"), [], [ Ok "lifecycle", Ok "failure" ] )
                    ( field "name" after, List.map .requestId after.pending, List.map (D.decodeValue (D.field "type" D.string)) events )
            )
        ]
