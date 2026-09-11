port module Data.QueryManager exposing (Incoming(..), Model, Msg(..), OptimisticMutation, OptimisticSetField, OptimisticWhere, QueryClientIncoming(..), QueryDeltaOp(..), QuerySubscription, ReExecuteDecision(..), decodeIncoming, decodeQueryClientIncoming, doesChangeAffectWhereClause, extractChangedRowIds, extractWhereClauseFields, init, mutationResult, notifyTablesChanged, queryClientDelta, queryClientFull, receiveIncoming, receiveQueryClientIncoming, shouldReExecuteQuery, update)

import Data.Delta
import Data.Error
import Data.Identity exposing (Key)
import Data.Schema
import Data.Value exposing (Value)
import Db
import Db.Query
import Dict exposing (Dict)
import Json.Decode as Decode
import Json.Encode as Encode
import Set exposing (Set)


type alias Model =
    { subscriptions : Dict String QuerySubscription }


type alias QuerySubscription =
    { queryId : String
    , query : Db.Query.Query
    , input : Encode.Value
    , callbackPort : String
    , resultRowIds : Dict String (Set Key)
    , revision : Int
    , lastResult : Maybe (Dict String (List (Dict String Value)))
    }


type Msg
    = IncomingReceived Incoming


type Incoming
    = SendMutation String String String (List ( String, String )) String Bool Encode.Value (Maybe OptimisticMutation)


type alias OptimisticMutation =
    { queryField : String, where_ : OptimisticWhere, set : List OptimisticSetField }


type alias OptimisticWhere =
    { field : String, input : String }


type alias OptimisticSetField =
    { field : String, input : String }


type QueryClientIncoming
    = QCRegister String Db.Query.Query Encode.Value
    | QCUpdateInput String (Maybe Db.Query.Query) Encode.Value
    | QCUnregister String


type QueryDeltaOp
    = SetRow String (Dict String Value)
    | RemoveRow String
    | InsertRow String Int (Dict String Value)
    | MoveRow String Int Int
    | RemoveRowByIndex String Int


init : Model
init =
    { subscriptions = Dict.empty }


update : Msg -> Model -> ( Model, Cmd Msg )
update _ model =
    ( model, Cmd.none )


{-| Results are arbitrary projections, not normalized entities. Until ordered
identity provenance is carried alongside every projection, publish full results
rather than guessing identity from a field called `id` (which may be an alias).
Unchanged results do not advance the subscription revision.
-}
notifyTablesChanged : Data.Schema.SchemaMetadata -> Db.Db -> Model -> Data.Delta.Delta -> ( Model, List (Cmd msg) )
notifyTablesChanged schema db model delta =
    case Data.Delta.validate schema delta of
        Err error ->
            ( model, [ Data.Error.sendError error ] )

        Ok _ ->
            Dict.foldl
                (\queryId subscription ( acc, cmds ) ->
                    case shouldReExecuteQuery schema db subscription delta of
                        NoReExecute ->
                            ( acc, cmds )

                        ReExecuteFull ->
                            let
                                result =
                                    Db.executeQueryWithTracking schema db subscription.query

                                changed =
                                    subscription.lastResult /= Just result.results

                                revision =
                                    subscription.revision
                                        + (if changed then
                                            1

                                           else
                                            0
                                          )

                                next =
                                    { subscription | resultRowIds = result.rowIds, lastResult = Just result.results, revision = revision }
                            in
                            ( { acc | subscriptions = Dict.insert queryId next acc.subscriptions }
                            , if changed then
                                queryClientFull queryId revision (encodeQueryResult result.results) :: cmds

                              else
                                cmds
                            )
                )
                ( model, [] )
                model.subscriptions


encodeQueryResult : Dict String (List (Dict String Value)) -> Encode.Value
encodeQueryResult =
    Encode.dict identity (Encode.list (Encode.dict identity Data.Value.encodeValue))


extractChangedRowIds : Data.Schema.SchemaMetadata -> Data.Delta.Delta -> Result String (Dict String (Set Key))
extractChangedRowIds =
    Data.Delta.validate


type ReExecuteDecision
    = NoReExecute
    | ReExecuteFull


{-| Table dependency tracking includes nested relationships and every query alias.
Re-evaluate on any dependent table change: comparing only WHERE or SORT fields
misses selected-field changes, and the caller may already have installed the delta.
-}
shouldReExecuteQuery : Data.Schema.SchemaMetadata -> Db.Db -> QuerySubscription -> Data.Delta.Delta -> ReExecuteDecision
shouldReExecuteQuery schema _ subscription delta =
    let
        dependencies table fieldQuery =
            Dict.foldl
                (\field selection found ->
                    case selection of
                        Db.Query.SelectField _ ->
                            found

                        Db.Query.SelectNested source nested ->
                            case Dict.get table schema.tables |> Maybe.andThen (.links >> Dict.get (Maybe.withDefault field source)) of
                                Just link ->
                                    Set.union found (dependencies link.to.table nested)

                                Nothing ->
                                    found
                )
                (Set.singleton table)
                fieldQuery.selections

        tables =
            Dict.foldl
                (\field query acc ->
                    case Dict.get field schema.queryFieldToTable of
                        Just table ->
                            Set.union acc (dependencies table query)

                        Nothing ->
                            acc
                )
                Set.empty
                subscription.query
    in
    if List.any (\group -> Set.member group.tableName tables && not (List.isEmpty group.rows)) delta.tableGroups then
        ReExecuteFull

    else
        NoReExecute


extractWhereClauseFields : Db.Query.WhereClause -> Set String
extractWhereClauseFields whereClause =
    Dict.foldl
        (\field value acc ->
            case value of
                Db.Query.FilterValueAnd clauses ->
                    List.foldl (extractWhereClauseFields >> Set.union) acc clauses

                Db.Query.FilterValueOr clauses ->
                    List.foldl (extractWhereClauseFields >> Set.union) acc clauses

                _ ->
                    Set.insert field acc
        )
        Set.empty
        whereClause


doesChangeAffectWhereClause : Db.Query.WhereClause -> Dict String Value -> Dict String Value -> Bool
doesChangeAffectWhereClause whereClause oldRow newRow =
    extractWhereClauseFields whereClause
        |> Set.toList
        |> List.any (\field -> Dict.get field oldRow /= Dict.get field newRow)


port queryManagerOut : Encode.Value -> Cmd msg


port receiveQueryManagerMessage : (Decode.Value -> msg) -> Sub msg


port queryClientOut : Encode.Value -> Cmd msg


port receiveQueryClientMessage : (Decode.Value -> msg) -> Sub msg


decodeIncoming : Decode.Decoder Incoming
decodeIncoming =
    Decode.field "type" Decode.string
        |> Decode.andThen
            (\type_ ->
                case type_ of
                    "sendMutation" ->
                        Decode.map8 SendMutation
                            (Decode.field "requestId" Decode.string)
                            (Decode.field "mutationId" Decode.string)
                            (Decode.field "baseUrl" Decode.string)
                            (Decode.oneOf [ Decode.field "headers" (Decode.list (Decode.map2 Tuple.pair (Decode.index 0 Decode.string) (Decode.index 1 Decode.string))), Decode.succeed [] ])
                            (Decode.oneOf [ Decode.field "credentials" Decode.string, Decode.succeed "same-origin" ])
                            (Decode.oneOf [ Decode.field "withCredentials" Decode.bool, Decode.succeed False ])
                            (Decode.field "input" Decode.value)
                            (Decode.maybe (Decode.field "optimistic" decodeOptimisticMutation))

                    _ ->
                        Decode.fail ("Unknown QueryManager incoming type: " ++ type_)
            )


decodeOptimisticMutation : Decode.Decoder OptimisticMutation
decodeOptimisticMutation =
    Decode.map3 OptimisticMutation
        (Decode.field "queryField" Decode.string)
        (Decode.field "where" (Decode.map2 OptimisticWhere (Decode.field "field" Decode.string) (Decode.field "input" Decode.string)))
        (Decode.field "set" (Decode.list (Decode.map2 OptimisticSetField (Decode.field "field" Decode.string) (Decode.field "input" Decode.string))))


decodeQueryClientIncoming : Decode.Decoder QueryClientIncoming
decodeQueryClientIncoming =
    Decode.field "type" Decode.string
        |> Decode.andThen
            (\type_ ->
                case type_ of
                    "register" ->
                        Decode.map3 QCRegister
                            (Decode.field "queryId" Decode.string)
                            (Decode.field "querySource" Db.Query.decodeQuery)
                            (Decode.field "queryInput" Decode.value)

                    "update-input" ->
                        Decode.map3 QCUpdateInput
                            (Decode.field "queryId" Decode.string)
                            (Decode.dict Decode.value
                                |> Decode.andThen
                                    (\fields ->
                                        if Dict.member "querySource" fields then
                                            Decode.field "querySource" Db.Query.decodeQuery |> Decode.map Just

                                        else
                                            Decode.succeed Nothing
                                    )
                            )
                            (Decode.field "queryInput" Decode.value)

                    "unregister" ->
                        Decode.map QCUnregister (Decode.field "queryId" Decode.string)

                    _ ->
                        Decode.fail ("Unknown QueryClient incoming type: " ++ type_)
            )


queryClientFull : String -> Int -> Encode.Value -> Cmd msg
queryClientFull queryId revision result =
    queryClientOut (Encode.object [ ( "type", Encode.string "full" ), ( "queryId", Encode.string queryId ), ( "revision", Encode.int revision ), ( "result", result ) ])


queryClientDelta : String -> Int -> List QueryDeltaOp -> Cmd msg
queryClientDelta queryId revision ops =
    queryClientOut (Encode.object [ ( "type", Encode.string "delta" ), ( "queryId", Encode.string queryId ), ( "revision", Encode.int revision ), ( "delta", Encode.object [ ( "ops", Encode.list encodeQueryDeltaOp ops ) ] ) ])


encodeQueryDeltaOp : QueryDeltaOp -> Encode.Value
encodeQueryDeltaOp op =
    let
        encode name path fields =
            Encode.object (( "op", Encode.string name ) :: ( "path", Encode.string path ) :: fields)

        rowField row =
            ( "row", Encode.dict identity Data.Value.encodeValue row )
    in
    case op of
        SetRow path row ->
            encode "set-row" path [ rowField row ]

        RemoveRow path ->
            encode "remove-row" path []

        InsertRow path index row ->
            encode "insert-row" path [ ( "index", Encode.int index ), rowField row ]

        MoveRow path from to ->
            encode "move-row" path [ ( "from", Encode.int from ), ( "to", Encode.int to ) ]

        RemoveRowByIndex path index ->
            encode "remove-row-by-index" path [ ( "index", Encode.int index ) ]


mutationResult : String -> String -> Result String Encode.Value -> Cmd msg
mutationResult requestId mutationId result =
    queryManagerOut
        (Encode.object
            [ ( "type", Encode.string "mutationResult" )
            , ( "requestId", Encode.string requestId )
            , ( "mutationId", Encode.string mutationId )
            , ( "result"
              , case result of
                    Ok value ->
                        Encode.object [ ( "ok", Encode.bool True ), ( "value", value ) ]

                    Err error ->
                        Encode.object [ ( "ok", Encode.bool False ), ( "error", Encode.string error ) ]
              )
            ]
        )


receiveIncoming : (Result Decode.Error Incoming -> msg) -> Sub msg
receiveIncoming toMsg =
    receiveQueryManagerMessage (Decode.decodeValue decodeIncoming >> toMsg)


receiveQueryClientIncoming : (Result Decode.Error QueryClientIncoming -> msg) -> Sub msg
receiveQueryClientIncoming toMsg =
    receiveQueryClientMessage (Decode.decodeValue decodeQueryClientIncoming >> toMsg)
