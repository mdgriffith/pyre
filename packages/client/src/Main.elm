port module Main exposing (main)

import Data.Catchup as Catchup
import Data.Delta
import Data.Error
import Data.IndexedDb as IndexedDb exposing (Incoming(..))
import Data.LiveSync as LiveSync exposing (Incoming(..))
import Data.QueryManager as QueryManager exposing (Incoming(..), Msg(..))
import Data.Schema
import Data.SyncState as SyncState
import Data.Value
import Db exposing (Msg(..))
import Db.Query
import Dict exposing (Dict)
import Http
import Json.Decode as Decode
import Json.Encode as Encode
import Platform
import String



-- Flags


type alias Flags =
    { schema : Data.Schema.SchemaMetadata
    , server : Catchup.ServerConfig
    , liveSync : LiveSync.Config
    , sync : SyncConfig
    }


type alias SyncConfig =
    { autoStart : Bool
    }



-- Model


type alias Model =
    { schema : Data.Schema.SchemaMetadata
    , db : Db.Db
    , queryManager : QueryManager.Model
    , catchup : Catchup.Model
    , syncStatus : SyncState.SyncStatus
    , tableSyncStatuses : Dict String SyncState.TableSyncStatus
    , syncError : Maybe String
    , liveSyncStarted : Bool
    , liveSyncTransport : LiveSync.Transport
    , syncRequested : Bool
    , inFlightOptimistic : Dict String OptimisticInFlight
    , optimisticOrder : List String
    , lastAppliedServerRevision : Maybe Int
    }


type alias OptimisticInFlight =
    { forward : Data.Delta.Delta
    , inverse : Data.Delta.Delta
    , acknowledgedServerRevision : Maybe Int
    }


type alias MutationSyncMessage =
    { serverRevision : Maybe Int
    , delta : Maybe Data.Delta.Delta
    , requiresCatchup : Bool
    }



-- Msg


type Msg
    = IndexedDbReceived IndexedDb.Incoming
    | LiveSyncReceived LiveSync.Incoming
    | QueryManagerReceived QueryManager.Incoming
    | QueryClientReceived QueryManager.QueryClientIncoming
    | MutationRequest String String String Encode.Value (Result Http.Error Encode.Value)
    | DbMsg Db.Msg
    | Error String
    | CatchupMsg Catchup.Msg
    | SyncControlReceived SyncControlMessage


type SyncControlMessage
    = StartSync



-- Init


init : Flags -> ( Model, Cmd Msg )
init flags =
    ( { schema = flags.schema
      , db = Db.init
      , queryManager = QueryManager.init
      , catchup = Catchup.init flags.server
      , syncStatus = SyncState.NotStarted
      , tableSyncStatuses = SyncState.initialTableStatuses flags.schema.tables
      , syncError = Nothing
      , liveSyncStarted = False
      , liveSyncTransport = flags.liveSync.transport
      , syncRequested = flags.sync.autoStart
      , inFlightOptimistic = Dict.empty
      , optimisticOrder = []
      , lastAppliedServerRevision = Nothing
      }
    , Cmd.batch
        [ if flags.sync.autoStart then
            IndexedDb.requestInitialData

          else
            Cmd.none
        , debugCmd "init"
            [ ( "autoStart", Encode.bool flags.sync.autoStart )
            , ( "transport", Encode.string (liveSyncTransportToString flags.liveSync.transport) )
            ]
        , emitSyncState
            { status = SyncState.NotStarted
            , tables = SyncState.initialTableStatuses flags.schema.tables
            }
        ]
    )



-- Update


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        IndexedDbReceived incoming ->
            case incoming of
                IndexedDb.InitialDataReceived _ ->
                    let
                        ( updatedDb, dbCmd ) =
                            Db.update (Db.FromIndexedDb model.schema incoming) model.db

                        baseModel =
                            { model | db = updatedDb }

                        ( updatedModel, indexedDbCmd ) =
                            handleIndexedDbIncoming incoming baseModel
                    in
                    ( updatedModel
                    , Cmd.batch [ Cmd.map DbMsg dbCmd, indexedDbCmd ]
                    )

                IndexedDb.DatabaseEpochResetCompleted databaseEpoch ->
                    applyCatchupUpdate
                        (Catchup.update (Catchup.DatabaseEpochResetCompleted databaseEpoch) model.catchup model.db)
                        model

                IndexedDb.DatabaseEpochResetFailed databaseEpoch message ->
                    applyCatchupUpdate
                        (Catchup.update (Catchup.DatabaseEpochResetFailed databaseEpoch message) model.catchup model.db)
                        model

        LiveSyncReceived incoming ->
            let
                ( updatedModel, cmd ) =
                    handleLiveSyncIncoming incoming model
            in
            ( updatedModel
            , Cmd.batch
                [ debugCmd "live-sync-received"
                    [ ( "messageType", Encode.string (liveSyncIncomingToString incoming) ) ]
                , cmd
                ]
            )

        QueryManagerReceived incoming ->
            let
                ( updatedQueryManager, _ ) =
                    QueryManager.update (QueryManager.IncomingReceived incoming) model.queryManager

                ( updatedModel, queryCmds ) =
                    handleQueryManagerIncoming incoming { model | queryManager = updatedQueryManager }
            in
            ( updatedModel
            , Cmd.batch queryCmds
            )

        QueryClientReceived incoming ->
            let
                ( updatedModel, queryCmds ) =
                    handleQueryClientIncoming incoming model
            in
            ( updatedModel
            , Cmd.batch queryCmds
            )

        MutationRequest requestId mutationId _ _ result ->
            case result of
                Ok response ->
                    settleSuccessfulMutation requestId mutationId response model

                Err error ->
                    rollbackOptimisticMutation requestId mutationId (httpErrorToString error) model

        Error errorMessage ->
            ( model
            , Data.Error.sendError errorMessage
            )

        CatchupMsg catchupMsg ->
            applyCatchupUpdate (Catchup.update catchupMsg model.catchup model.db) model

        SyncControlReceived StartSync ->
            if model.syncRequested then
                ( model, Cmd.none )

            else
                ( { model | syncRequested = True }
                , Cmd.batch
                    [ debugCmd "sync-control-start" []
                    , IndexedDb.requestInitialData
                    ]
                )

        DbMsg dbMsg ->
            let
                ( updatedDb, dbCmd ) =
                    Db.update dbMsg model.db
            in
            ( { model | db = updatedDb }
            , Cmd.map DbMsg dbCmd
            )


handleIndexedDbIncoming : IndexedDb.Incoming -> Model -> ( Model, Cmd Msg )
handleIndexedDbIncoming incoming model =
    case incoming of
        IndexedDb.InitialDataReceived initialData ->
            let
                ( updatedQueryManager, cmds ) =
                    reExecuteAllQueries model.schema model.db model.queryManager

                baseModel =
                    { model
                        | queryManager = updatedQueryManager
                        , lastAppliedServerRevision = initialData.lastAppliedServerRevision
                    }

                ( catchupModel, catchupCmd ) =
                    applyCatchupUpdate (Catchup.update (Catchup.InitialDataLoaded initialData.cursor initialData.databaseEpoch) model.catchup model.db) baseModel
            in
            ( catchupModel
            , Cmd.batch [ Cmd.batch cmds, catchupCmd ]
            )

        IndexedDb.DatabaseEpochResetCompleted _ ->
            ( model, Cmd.none )

        IndexedDb.DatabaseEpochResetFailed _ _ ->
            ( model, Cmd.none )


handleLiveSyncIncoming : LiveSync.Incoming -> Model -> ( Model, Cmd Msg )
handleLiveSyncIncoming incoming model =
    case incoming of
        LiveSync.DeltaReceived messageDatabaseId messageEpoch serverRevision delta ->
            case validateLiveSyncDatabaseId model messageDatabaseId "delta" of
                Just message ->
                    ( { model | syncError = Just message }
                    , Cmd.batch
                        [ emitSyncState (toSyncState model)
                        , Data.Error.sendError message
                        ]
                    )

                Nothing ->
                    if liveEpochMismatch model messageEpoch then
                        applyCatchupUpdate (Catchup.update Catchup.CatchupRequired model.catchup model.db) model

                    else if isStaleServerRevision serverRevision model.lastAppliedServerRevision then
                        ( model, Cmd.none )

                    else
                        let
                            ( updatedDb, dbCmds ) =
                                applyAuthoritativeDelta delta model

                            ( updatedQueryManager, triggerCmds ) =
                                QueryManager.notifyTablesChanged model.schema updatedDb model.queryManager delta
                        in
                        ( { model
                            | db = updatedDb
                            , queryManager = updatedQueryManager
                            , lastAppliedServerRevision = updateLastAppliedServerRevision serverRevision model.lastAppliedServerRevision
                          }
                        , Cmd.batch
                            [ Cmd.batch (List.map (Cmd.map DbMsg) dbCmds)
                            , Cmd.batch triggerCmds
                            , writeServerRevisionCmd serverRevision
                            ]
                        )

        LiveSync.LiveSyncConnected messageDatabaseId messageEpoch _ ->
            case validateLiveSyncDatabaseId model messageDatabaseId "connected" of
                Just message ->
                    ( { model | syncError = Just message }
                    , Data.Error.sendError message
                    )

                Nothing ->
                    if liveEpochMismatch model messageEpoch then
                        applyCatchupUpdate (Catchup.update Catchup.CatchupRequired model.catchup model.db) model

                    else
                        ( model, Cmd.none )

        LiveSync.LiveSyncError error ->
            ( { model | syncError = Just error }
            , Cmd.batch
                [ emitSyncState (toSyncState model)
                , Data.Error.sendError error
                ]
            )

        LiveSync.SyncProgressReceived messageDatabaseId _ ->
            case validateLiveSyncDatabaseId model messageDatabaseId "syncProgress" of
                Just message ->
                    ( { model | syncError = Just message }
                    , Data.Error.sendError message
                    )

                Nothing ->
                    let
                        updatedModel =
                            { model | syncStatus = SyncState.CatchingUp }
                    in
                    ( updatedModel
                    , emitSyncState (toSyncState updatedModel)
                    )

        LiveSync.SyncCompleteReceived messageDatabaseId ->
            case validateLiveSyncDatabaseId model messageDatabaseId "syncComplete" of
                Just message ->
                    ( { model | syncError = Just message }
                    , Data.Error.sendError message
                    )

                Nothing ->
                    let
                        updatedModel =
                            { model
                                | syncStatus = SyncState.Live
                                , tableSyncStatuses = SyncState.markAllTablesLive model.tableSyncStatuses
                                , syncError = Nothing
                            }
                    in
                    ( updatedModel
                    , emitSyncState (toSyncState updatedModel)
                    )

        LiveSync.SyncRequiredReceived messageDatabaseId messageEpoch serverRevision ->
            case validateLiveSyncDatabaseId model messageDatabaseId "syncRequired" of
                Just message ->
                    ( { model | syncError = Just message }
                    , Data.Error.sendError message
                    )

                Nothing ->
                    if not (liveEpochMismatch model messageEpoch) then
                        if isStaleServerRevision serverRevision model.lastAppliedServerRevision then
                            ( model, Cmd.none )

                        else
                            applyCatchupUpdate (Catchup.update Catchup.CatchupRequired model.catchup model.db) model

                    else
                        applyCatchupUpdate (Catchup.update Catchup.CatchupRequired model.catchup model.db) model


liveEpochMismatch : Model -> Maybe String -> Bool
liveEpochMismatch model messageEpoch =
    case ( Catchup.databaseEpoch model.catchup, messageEpoch ) of
        ( Just currentEpoch, Just incomingEpoch ) ->
            currentEpoch /= incomingEpoch

        _ ->
            False


validateLiveSyncDatabaseId : Model -> Maybe String -> String -> Maybe String
validateLiveSyncDatabaseId model actual eventName =
    case Catchup.databaseId model.catchup of
        Nothing ->
            Nothing

        Just expected ->
            case actual of
                Just actualId ->
                    if actualId == expected then
                        Nothing

                    else
                        Just ("Live sync " ++ eventName ++ " databaseId mismatch: expected " ++ expected ++ ", got " ++ actualId)

                Nothing ->
                    Just ("Live sync " ++ eventName ++ " missing databaseId: expected " ++ expected)


handleQueryManagerIncoming : QueryManager.Incoming -> Model -> ( Model, List (Cmd Msg) )
handleQueryManagerIncoming incoming model =
    case incoming of
        QueryManager.SendMutation requestId mutationId baseUrl headers credentials withCredentials input optimistic ->
            -- Mutations are handled via HTTP request
            let
                ( optimisticModel, optimisticCmds ) =
                    applyOptimisticMutation requestId optimistic input model

                url =
                    buildMutationUrl baseUrl mutationId

                request =
                    { method = "POST"
                    , headers = List.map (\( key, value ) -> Http.header key value) headers
                    , url = url
                    , body = Http.jsonBody input
                    , expect =
                        Http.expectStringResponse
                            (MutationRequest requestId mutationId baseUrl input)
                            (\response ->
                                case response of
                                    Http.BadUrl_ badUrl ->
                                        Err (Http.BadUrl badUrl)

                                    Http.Timeout_ ->
                                        Err Http.Timeout

                                    Http.NetworkError_ ->
                                        Err Http.NetworkError

                                    Http.BadStatus_ metadata body ->
                                        Err (Http.BadStatus metadata.statusCode)

                                    Http.GoodStatus_ _ body ->
                                        case Decode.decodeString Decode.value body of
                                            Ok json ->
                                                Ok json

                                            Err err ->
                                                Err (Http.BadBody (Decode.errorToString err))
                            )
                    , timeout = Nothing
                    , tracker = Nothing
                    }
            in
            ( optimisticModel
            , optimisticCmds
                ++ [ if credentials == "include" || withCredentials then
                        Http.riskyRequest request

                     else
                        Http.request request
                   ]
            )


handleQueryClientIncoming : QueryManager.QueryClientIncoming -> Model -> ( Model, List (Cmd Msg) )
handleQueryClientIncoming incoming model =
    case incoming of
        QueryManager.QCRegister queryId query input ->
            -- Register the query and execute it immediately
            let
                subscription =
                    QueryManager.QuerySubscription queryId query input "" Dict.empty 0 Nothing

                updatedSubscriptions =
                    Dict.insert queryId subscription model.queryManager.subscriptions

                executionResult =
                    Db.executeQueryWithTracking model.schema model.db query

                resultJson =
                    encodeQueryResult executionResult.results

                nextRevision =
                    1

                finalSubscription =
                    { subscription
                        | resultRowIds = executionResult.rowIds
                        , revision = nextRevision
                        , lastResult = Just executionResult.results
                    }

                finalSubscriptions =
                    Dict.insert queryId finalSubscription updatedSubscriptions

                updatedQueryManager =
                    { subscriptions = finalSubscriptions }
            in
            ( { model | queryManager = updatedQueryManager }
            , [ QueryManager.queryClientFull queryId nextRevision resultJson ]
            )

        QueryManager.QCUpdateInput queryId maybeQuery newInput ->
            -- Update the input and re-execute
            case Dict.get queryId model.queryManager.subscriptions of
                Just subscription ->
                    let
                        nextQuery =
                            Maybe.withDefault subscription.query maybeQuery

                        updatedSubscription =
                            { subscription
                                | query = nextQuery
                                , input = newInput
                                , resultRowIds = Dict.empty
                                , lastResult = Nothing
                            }

                        executionResult =
                            Db.executeQueryWithTracking model.schema model.db nextQuery

                        resultJson =
                            encodeQueryResult executionResult.results

                        nextRevision =
                            subscription.revision + 1

                        finalSubscription =
                            { updatedSubscription
                                | resultRowIds = executionResult.rowIds
                                , revision = nextRevision
                                , lastResult = Just executionResult.results
                            }

                        updatedSubscriptions =
                            Dict.insert queryId finalSubscription model.queryManager.subscriptions

                        updatedQueryManager =
                            { subscriptions = updatedSubscriptions }
                    in
                    ( { model | queryManager = updatedQueryManager }
                    , [ QueryManager.queryClientFull queryId nextRevision resultJson ]
                    )

                Nothing ->
                    ( model, [] )

        QueryManager.QCUnregister queryId ->
            let
                updatedSubscriptions =
                    Dict.remove queryId model.queryManager.subscriptions

                updatedQueryManager =
                    { subscriptions = updatedSubscriptions }
            in
            ( { model | queryManager = updatedQueryManager }
            , []
            )



-- Helper Functions


httpErrorToString : Http.Error -> String
httpErrorToString error =
    case error of
        Http.BadUrl url ->
            "Bad URL: " ++ url

        Http.Timeout ->
            "Timeout"

        Http.NetworkError ->
            "Network Error"

        Http.BadStatus code ->
            "Bad Status: " ++ String.fromInt code

        Http.BadBody message ->
            "Decode Error: " ++ message


buildMutationUrl : String -> String -> String
buildMutationUrl baseUrl id =
    case String.split "?" baseUrl of
        base :: queryParts ->
            let
                query =
                    String.join "?" queryParts
            in
            if String.isEmpty query then
                base ++ "/" ++ id

            else
                base ++ "/" ++ id ++ "?" ++ query

        [] ->
            baseUrl ++ "/" ++ id


applyOptimisticMutation : String -> Maybe QueryManager.OptimisticMutation -> Encode.Value -> Model -> ( Model, List (Cmd Msg) )
applyOptimisticMutation requestId maybeOptimistic input model =
    case maybeOptimistic of
        Nothing ->
            ( model, [] )

        Just optimistic ->
            case Decode.decodeValue (Decode.dict Data.Value.decodeValue) input of
                Err _ ->
                    ( model, [] )

                Ok inputValues ->
                    case Dict.get optimistic.where_.input inputValues of
                        Nothing ->
                            ( model, [] )

                        Just whereValue ->
                            let
                                tableName =
                                    Dict.get optimistic.queryField model.schema.queryFieldToTable
                                        |> Maybe.withDefault optimistic.queryField

                                setValues =
                                    optimistic.set
                                        |> List.filterMap
                                            (\setField ->
                                                Dict.get setField.input inputValues
                                                    |> Maybe.map (\value -> ( setField.field, value ))
                                            )

                                matchingRows =
                                    Dict.get tableName model.db.tables
                                        |> Maybe.withDefault Dict.empty
                                        |> Dict.values
                                        |> List.filter
                                            (\row -> Dict.get optimistic.where_.field row == Just whereValue)
                            in
                            if List.isEmpty setValues || List.isEmpty matchingRows then
                                ( model, [] )

                            else
                                let
                                    updatedRows =
                                        List.map (applySetValues setValues) matchingRows

                                    forward =
                                        deltaFromRows tableName updatedRows

                                    inverse =
                                        deltaFromRows tableName matchingRows

                                    ( updatedDb, dbCmd ) =
                                        Db.update (Db.LocalDeltaReceived forward) model.db

                                    ( updatedQueryManager, triggerCmds ) =
                                        QueryManager.notifyTablesChanged model.schema updatedDb model.queryManager forward
                                in
                                ( { model
                                    | db = updatedDb
                                    , queryManager = updatedQueryManager
                                    , inFlightOptimistic = Dict.insert requestId { forward = forward, inverse = inverse, acknowledgedServerRevision = Nothing } model.inFlightOptimistic
                                    , optimisticOrder = appendUnique requestId model.optimisticOrder
                                  }
                                , Cmd.map DbMsg dbCmd :: triggerCmds
                                )


rollbackOptimisticMutation : String -> String -> String -> Model -> ( Model, Cmd Msg )
rollbackOptimisticMutation requestId mutationId error model =
    case Dict.get requestId model.inFlightOptimistic of
        Nothing ->
            ( removeOptimisticMutation requestId model
            , QueryManager.mutationResult requestId mutationId (Err error)
            )

        Just optimistic ->
            let
                ( updatedDb, dbCmd ) =
                    Db.update (Db.LocalDeltaReceived optimistic.inverse) model.db

                ( updatedQueryManager, triggerCmds ) =
                    QueryManager.notifyTablesChanged model.schema updatedDb model.queryManager optimistic.inverse

                cleanedModel =
                    removeOptimisticMutation requestId model
            in
            ( { cleanedModel
                | db = updatedDb
                , queryManager = updatedQueryManager
              }
            , Cmd.batch
                (QueryManager.mutationResult requestId mutationId (Err error)
                    :: Cmd.map DbMsg dbCmd
                    :: triggerCmds
                )
            )


settleSuccessfulMutation : String -> String -> Encode.Value -> Model -> ( Model, Cmd Msg )
settleSuccessfulMutation requestId mutationId response model =
    let
        serverRevision =
            extractServerRevision response

        maybeSyncMessage =
            extractMutationSyncMessage response
    in
    if Dict.member requestId model.inFlightOptimistic && missingAuthoritativeMutationEnvelope serverRevision maybeSyncMessage then
        rollbackOptimisticMutation requestId mutationId "Optimistic mutation response missing authoritative sync envelope" model

    else
        settleSuccessfulMutationWithEnvelope requestId mutationId response serverRevision maybeSyncMessage model


missingAuthoritativeMutationEnvelope : Maybe Int -> Maybe MutationSyncMessage -> Bool
missingAuthoritativeMutationEnvelope serverRevision maybeSyncMessage =
    case ( serverRevision, maybeSyncMessage ) of
        ( Just _, Just _ ) ->
            False

        _ ->
            True


settleSuccessfulMutationWithEnvelope : String -> String -> Encode.Value -> Maybe Int -> Maybe MutationSyncMessage -> Model -> ( Model, Cmd Msg )
settleSuccessfulMutationWithEnvelope requestId mutationId response serverRevision maybeSyncMessage model =
    let
        shouldApplyAuthoritative =
            not (isStaleServerRevision serverRevision model.lastAppliedServerRevision)

        ( authoritativeModel, authoritativeDbCmds, authoritativeQueryCmds ) =
            case maybeSyncMessage of
                Just syncMessage ->
                    case syncMessage.delta of
                        Just delta ->
                            if shouldApplyAuthoritative then
                                let
                                    ( updatedDb, dbCmds ) =
                                        applyAuthoritativeDelta delta model

                                    ( updatedQueryManager, triggerCmds ) =
                                        QueryManager.notifyTablesChanged model.schema updatedDb model.queryManager delta
                                in
                                ( { model | db = updatedDb, queryManager = updatedQueryManager }, dbCmds, triggerCmds )

                            else
                                ( model, [], [] )

                        Nothing ->
                            ( model, [], [] )

                Nothing ->
                    ( model, [], [] )

        updatedModel =
            case serverRevision of
                Nothing ->
                    removeOptimisticMutation requestId authoritativeModel

                Just revision ->
                    authoritativeModel
                        |> acknowledgeOptimisticMutation requestId revision
                        |> updateModelLastAppliedServerRevision serverRevision
                        |> pruneAcknowledgedOptimisticPrefix

        ( finalModel, catchupCmd ) =
            case maybeSyncMessage of
                Just syncMessage ->
                    if syncMessage.requiresCatchup && shouldApplyAuthoritative then
                        applyCatchupUpdate (Catchup.update Catchup.CatchupRequired updatedModel.catchup updatedModel.db) updatedModel

                    else
                        ( updatedModel, Cmd.none )

                Nothing ->
                    ( updatedModel, Cmd.none )
    in
    ( finalModel
    , Cmd.batch
        [ QueryManager.mutationResult requestId mutationId (Ok response)
        , writeServerRevisionCmd serverRevision
        , Cmd.batch (List.map (Cmd.map DbMsg) authoritativeDbCmds)
        , Cmd.batch authoritativeQueryCmds
        , catchupCmd
        ]
    )


acknowledgeOptimisticMutation : String -> Int -> Model -> Model
acknowledgeOptimisticMutation requestId serverRevision model =
    { model
        | inFlightOptimistic =
            Dict.update requestId
                (Maybe.map (\optimistic -> { optimistic | acknowledgedServerRevision = Just serverRevision }))
                model.inFlightOptimistic
    }


updateModelLastAppliedServerRevision : Maybe Int -> Model -> Model
updateModelLastAppliedServerRevision serverRevision model =
    { model | lastAppliedServerRevision = updateLastAppliedServerRevision serverRevision model.lastAppliedServerRevision }


pruneAcknowledgedOptimisticPrefix : Model -> Model
pruneAcknowledgedOptimisticPrefix model =
    case model.optimisticOrder of
        [] ->
            model

        requestId :: rest ->
            case Dict.get requestId model.inFlightOptimistic of
                Just optimistic ->
                    case optimistic.acknowledgedServerRevision of
                        Just _ ->
                            pruneAcknowledgedOptimisticPrefix
                                { model
                                    | inFlightOptimistic = Dict.remove requestId model.inFlightOptimistic
                                    , optimisticOrder = rest
                                }

                        Nothing ->
                            model

                Nothing ->
                    pruneAcknowledgedOptimisticPrefix { model | optimisticOrder = rest }


applyAuthoritativeDelta : Data.Delta.Delta -> Model -> ( Db.Db, List (Cmd Db.Msg) )
applyAuthoritativeDelta delta model =
    let
        ( authoritativeDb, authoritativeCmd ) =
            Db.update (Db.DeltaReceived delta) model.db

        ( replayedDb, replayCmds ) =
            replayOptimisticMutations model authoritativeDb
    in
    ( replayedDb, authoritativeCmd :: replayCmds )


replayOptimisticMutations : Model -> Db.Db -> ( Db.Db, List (Cmd Db.Msg) )
replayOptimisticMutations model db =
    model.optimisticOrder
        |> List.foldl
            (\requestId ( currentDb, cmds ) ->
                case Dict.get requestId model.inFlightOptimistic of
                    Nothing ->
                        ( currentDb, cmds )

                    Just optimistic ->
                        let
                            ( nextDb, cmd ) =
                                Db.update (Db.LocalDeltaReceived optimistic.forward) currentDb
                        in
                        ( nextDb, cmd :: cmds )
            )
            ( db, [] )
        |> Tuple.mapSecond List.reverse


removeOptimisticMutation : String -> Model -> Model
removeOptimisticMutation requestId model =
    { model
        | inFlightOptimistic = Dict.remove requestId model.inFlightOptimistic
        , optimisticOrder = List.filter ((/=) requestId) model.optimisticOrder
    }


isStaleServerRevision : Maybe Int -> Maybe Int -> Bool
isStaleServerRevision serverRevision lastAppliedServerRevision =
    case ( serverRevision, lastAppliedServerRevision ) of
        ( Just incomingRevision, Just appliedRevision ) ->
            incomingRevision <= appliedRevision

        _ ->
            False


updateLastAppliedServerRevision : Maybe Int -> Maybe Int -> Maybe Int
updateLastAppliedServerRevision serverRevision lastAppliedServerRevision =
    case serverRevision of
        Nothing ->
            lastAppliedServerRevision

        Just incomingRevision ->
            case lastAppliedServerRevision of
                Nothing ->
                    Just incomingRevision

                Just appliedRevision ->
                    Just (max incomingRevision appliedRevision)


extractServerRevision : Encode.Value -> Maybe Int
extractServerRevision value =
    case Decode.decodeValue (Decode.field "serverRevision" Decode.int) value of
        Ok revision ->
            Just revision

        Err _ ->
            Nothing


extractMutationSyncMessage : Encode.Value -> Maybe MutationSyncMessage
extractMutationSyncMessage value =
    case Decode.decodeValue (Decode.field "sync" decodeMutationSyncMessage) value of
        Ok syncMessage ->
            Just syncMessage

        Err _ ->
            Nothing


decodeMutationSyncMessage : Decode.Decoder MutationSyncMessage
decodeMutationSyncMessage =
    Decode.field "type" Decode.string
        |> Decode.andThen
            (\type_ ->
                case type_ of
                    "delta" ->
                        Decode.map2
                            (\serverRevision delta ->
                                { serverRevision = serverRevision
                                , delta = Just delta
                                , requiresCatchup = False
                                }
                            )
                            (Decode.maybe (Decode.field "serverRevision" Decode.int))
                            (Decode.field "data" Data.Delta.decodeDelta)

                    "syncRequired" ->
                        Decode.map
                            (\serverRevision ->
                                { serverRevision = serverRevision
                                , delta = Nothing
                                , requiresCatchup = True
                                }
                            )
                            (Decode.maybe (Decode.field "serverRevision" Decode.int))

                    "catchupRequired" ->
                        Decode.map
                            (\serverRevision ->
                                { serverRevision = serverRevision
                                , delta = Nothing
                                , requiresCatchup = True
                                }
                            )
                            (Decode.maybe (Decode.field "serverRevision" Decode.int))

                    _ ->
                        Decode.fail ("Unknown mutation sync message type: " ++ type_)
            )


writeServerRevisionCmd : Maybe Int -> Cmd msg
writeServerRevisionCmd serverRevision =
    case serverRevision of
        Nothing ->
            Cmd.none

        Just revision ->
            IndexedDb.writeServerRevision revision


appendUnique : String -> List String -> List String
appendUnique value values =
    if List.member value values then
        values

    else
        values ++ [ value ]


applySetValues : List ( String, Data.Value.Value ) -> Dict String Data.Value.Value -> Dict String Data.Value.Value
applySetValues setValues row =
    List.foldl
        (\( field, value ) acc -> Dict.insert field value acc)
        row
        setValues


deltaFromRows : String -> List (Dict String Data.Value.Value) -> Data.Delta.Delta
deltaFromRows tableName rows =
    let
        headers =
            rows
                |> List.concatMap Dict.keys
                |> uniqueStrings

        rowValues row =
            List.map (\header -> Dict.get header row |> Maybe.withDefault Data.Value.NullValue) headers
    in
    { tableGroups =
        [ { tableName = tableName
          , headers = headers
          , rows = List.map rowValues rows
          }
        ]
    }


uniqueStrings : List String -> List String
uniqueStrings values =
    values
        |> List.foldl
            (\value acc ->
                if List.member value acc then
                    acc

                else
                    value :: acc
            )
            []
        |> List.reverse


applyCatchupUpdate : Catchup.UpdateResult -> Model -> ( Model, Cmd Msg )
applyCatchupUpdate result model =
    let
        nextSyncStatus =
            syncStatusFromCatchup result.model

        nextTableSyncStatuses =
            case nextSyncStatus of
                SyncState.NotStarted ->
                    model.tableSyncStatuses

                SyncState.CatchingUp ->
                    SyncState.markTablesCatchingUp result.touchedTables model.tableSyncStatuses

                SyncState.Live ->
                    SyncState.markAllTablesLive model.tableSyncStatuses

        ( replayedDb, replayDbCmds ) =
            if result.destructiveReset then
                ( result.db, [] )

            else
                case result.delta of
                    Just _ ->
                        replayOptimisticMutations model result.db

                    Nothing ->
                        ( result.db, [] )

        updatedModel =
            { model
                | catchup = result.model
                , db = replayedDb
                , syncStatus = nextSyncStatus
                , tableSyncStatuses = nextTableSyncStatuses
                , lastAppliedServerRevision =
                    if result.destructiveReset then
                        Nothing

                    else
                        updateLastAppliedServerRevision result.serverRevision model.lastAppliedServerRevision
                , inFlightOptimistic =
                    if result.destructiveReset then
                        Dict.empty

                    else
                        model.inFlightOptimistic
                , optimisticOrder =
                    if result.destructiveReset then
                        []

                    else
                        model.optimisticOrder
                , syncError =
                    case result.error of
                        Just message ->
                            Just message

                        Nothing ->
                            if nextSyncStatus == SyncState.Live then
                                Nothing

                            else
                                model.syncError
            }

        ( liveSyncModel, liveSyncCmd ) =
            startLiveSyncIfReady updatedModel

        ( updatedQueryManager, triggerCmds ) =
            if result.destructiveReset then
                reExecuteAllQueries model.schema replayedDb model.queryManager

            else
                case result.delta of
                    Just delta ->
                        QueryManager.notifyTablesChanged model.schema replayedDb model.queryManager delta

                    Nothing ->
                        ( model.queryManager, [] )

        errorCmd =
            case result.error of
                Just message ->
                    Data.Error.sendError message

                Nothing ->
                    Cmd.none

        dbCmds =
            result.dbCmds
                ++ replayDbCmds
                |> List.map (Cmd.map DbMsg)

        epochChangeCmd =
            case ( Catchup.databaseEpoch model.catchup, Catchup.pendingDatabaseEpoch result.model ) of
                ( Just fromEpoch, Just toEpoch ) ->
                    debugCmd "database-epoch-change"
                        [ ( "fromEpoch", Encode.string fromEpoch )
                        , ( "toEpoch", Encode.string toEpoch )
                        ]

                _ ->
                    Cmd.none

        cmds =
            [ Cmd.map CatchupMsg result.cmd
            , errorCmd
            , Cmd.batch triggerCmds
            , liveSyncCmd
            , if result.destructiveReset then
                Cmd.none

              else
                writeServerRevisionCmd result.serverRevision
            , emitSyncState (toSyncState liveSyncModel)
            , epochChangeCmd
            , debugCmd "catchup-update"
                [ ( "status", Encode.string (catchupStatusToString (Catchup.status result.model)) )
                , ( "touchedTables", Encode.list Encode.string result.touchedTables )
                , ( "hasDelta"
                  , Encode.bool
                        (case result.delta of
                            Just _ ->
                                True

                            Nothing ->
                                False
                        )
                  )
                , ( "dbCmdCount", Encode.int (List.length result.dbCmds) )
                ]
            ]
                ++ dbCmds
    in
    ( { liveSyncModel | queryManager = updatedQueryManager }
    , Cmd.batch cmds
    )


syncStatusFromCatchup : Catchup.Model -> SyncState.SyncStatus
syncStatusFromCatchup catchupModel =
    case Catchup.status catchupModel of
        Catchup.NotStarted ->
            SyncState.NotStarted

        Catchup.Syncing _ ->
            SyncState.CatchingUp

        Catchup.Synced ->
            SyncState.Live

        Catchup.Error _ ->
            SyncState.CatchingUp


toSyncState : Model -> SyncState.SyncState
toSyncState model =
    { status = model.syncStatus
    , tables = model.tableSyncStatuses
    }


emitSyncState : SyncState.SyncState -> Cmd Msg
emitSyncState syncState =
    syncStateOut (SyncState.encodeSyncState syncState)


port syncStateOut : Encode.Value -> Cmd msg


debugCmd : String -> List ( String, Encode.Value ) -> Cmd msg
debugCmd event fields =
    debugOut
        (Encode.object
            ([ ( "event", Encode.string event ) ] ++ fields)
        )


port debugOut : Encode.Value -> Cmd msg


startLiveSyncIfReady : Model -> ( Model, Cmd Msg )
startLiveSyncIfReady model =
    case ( model.liveSyncStarted, Catchup.status model.catchup ) of
        ( False, Catchup.Synced ) ->
            ( { model | liveSyncStarted = True }
            , Cmd.batch
                [ debugCmd "live-sync-connect"
                    [ ( "reason", Encode.string "catchup-synced" )
                    , ( "transport", Encode.string (liveSyncTransportToString model.liveSyncTransport) )
                    ]
                , LiveSync.connect
                    { transport = model.liveSyncTransport }
                ]
            )

        ( False, Catchup.Error _ ) ->
            ( { model | liveSyncStarted = True }
            , Cmd.batch
                [ debugCmd "live-sync-connect"
                    [ ( "reason", Encode.string "catchup-error" )
                    , ( "transport", Encode.string (liveSyncTransportToString model.liveSyncTransport) )
                    ]
                , LiveSync.connect
                    { transport = model.liveSyncTransport }
                ]
            )

        _ ->
            ( model
            , debugCmd "live-sync-not-ready"
                [ ( "liveSyncStarted", Encode.bool model.liveSyncStarted )
                , ( "catchupStatus", Encode.string (catchupStatusToString (Catchup.status model.catchup)) )
                ]
            )


catchupStatusToString : Catchup.Status -> String
catchupStatusToString status =
    case status of
        Catchup.NotStarted ->
            "not_started"

        Catchup.Syncing _ ->
            "syncing"

        Catchup.Synced ->
            "synced"

        Catchup.Error _ ->
            "error"


liveSyncTransportToString : LiveSync.Transport -> String
liveSyncTransportToString transport =
    case transport of
        LiveSync.Sse ->
            "sse"

        LiveSync.WebSocket ->
            "websocket"


liveSyncIncomingToString : LiveSync.Incoming -> String
liveSyncIncomingToString incoming =
    case incoming of
        LiveSync.DeltaReceived _ _ _ _ ->
            "delta"

        LiveSync.SyncProgressReceived _ _ ->
            "syncProgress"

        LiveSync.LiveSyncConnected _ _ _ ->
            "connected"

        LiveSync.LiveSyncError _ ->
            "error"

        LiveSync.SyncCompleteReceived _ ->
            "syncComplete"

        LiveSync.SyncRequiredReceived _ _ _ ->
            "syncRequired"


encodeQueryResult : Dict String (List (Dict String Data.Value.Value)) -> Encode.Value
encodeQueryResult result =
    Encode.dict identity
        (\rows ->
            Encode.list (\row -> Encode.dict identity Data.Value.encodeValue row) rows
        )
        result



-- Subscriptions


subscriptions : Model -> Sub Msg
subscriptions model =
    Sub.batch
        [ IndexedDb.receiveIncoming
            (\result ->
                case result of
                    Ok incoming ->
                        IndexedDbReceived incoming

                    Err err ->
                        -- Send error to console
                        Error ("Failed to decode IndexedDB message: " ++ Decode.errorToString err)
            )
        , LiveSync.receiveIncoming
            (\result ->
                case result of
                    Ok incoming ->
                        LiveSyncReceived incoming

                    Err err ->
                        -- Send error to console
                        Error ("Failed to decode LiveSync message: " ++ Decode.errorToString err)
            )
        , QueryManager.receiveIncoming
            (\result ->
                case result of
                    Ok incoming ->
                        QueryManagerReceived incoming

                    Err err ->
                        -- Send error to console
                        Error ("Failed to decode QueryManager message: " ++ Decode.errorToString err)
            )
        , QueryManager.receiveQueryClientIncoming
            (\result ->
                case result of
                    Ok incoming ->
                        QueryClientReceived incoming

                    Err err ->
                        -- Send error to console
                        Error ("Failed to decode QueryClient message: " ++ Decode.errorToString err)
            )
        , receiveSyncControlMessage
            (\jsonValue ->
                case Decode.decodeValue decodeSyncControlMessage jsonValue of
                    Ok incoming ->
                        SyncControlReceived incoming

                    Err err ->
                        Error ("Failed to decode sync control message: " ++ Decode.errorToString err)
            )
        ]



-- Main


main : Program Decode.Value Model Msg
main =
    Platform.worker
        { init =
            \flagsJson ->
                case Decode.decodeValue decodeFlags flagsJson of
                    Ok flags ->
                        init flags

                    Err err ->
                        -- Fallback with empty schema and default live sync config
                        init
                            { schema =
                                { tables = Dict.empty
                                , queryFieldToTable = Dict.empty
                                }
                            , server =
                                { baseUrl = ""
                                , catchupPath = ""
                                , databaseId = Nothing
                                , headers = []
                                , credentials = "same-origin"
                                , withCredentials = False
                                }
                            , liveSync =
                                { transport = LiveSync.Sse }
                            , sync =
                                { autoStart = True }
                            }
        , update = update
        , subscriptions = subscriptions
        }


decodeFlags : Decode.Decoder Flags
decodeFlags =
    Decode.map4 Flags
        (Decode.field "schema" Data.Schema.decodeSchemaMetadata)
        (Decode.field "server" decodeServerConfig)
        (Decode.oneOf
            [ Decode.field "liveSync" LiveSync.decodeConfig
            , Decode.succeed { transport = LiveSync.Sse }
            ]
        )
        (Decode.oneOf
            [ Decode.field "sync" decodeSyncConfig
            , Decode.succeed { autoStart = True }
            ]
        )


decodeSyncConfig : Decode.Decoder SyncConfig
decodeSyncConfig =
    Decode.map SyncConfig
        (Decode.field "autoStart" Decode.bool)


decodeSyncControlMessage : Decode.Decoder SyncControlMessage
decodeSyncControlMessage =
    Decode.field "type" Decode.string
        |> Decode.andThen
            (\type_ ->
                case type_ of
                    "startSync" ->
                        Decode.succeed StartSync

                    _ ->
                        Decode.fail ("Unknown sync control message type: " ++ type_)
            )


port receiveSyncControlMessage : (Decode.Value -> msg) -> Sub msg


decodeServerConfig : Decode.Decoder Catchup.ServerConfig
decodeServerConfig =
    Decode.map6 Catchup.ServerConfig
        (Decode.field "baseUrl" Decode.string)
        (Decode.field "catchupPath" Decode.string)
        (Decode.maybe (Decode.field "databaseId" Decode.string))
        (Decode.oneOf
            [ Decode.field "headers" decodeHeaders
            , Decode.succeed []
            ]
        )
        (Decode.oneOf
            [ Decode.field "credentials" Decode.string
            , Decode.succeed "same-origin"
            ]
        )
        (Decode.oneOf
            [ Decode.field "withCredentials" Decode.bool
            , Decode.succeed False
            ]
        )


decodeHeaders : Decode.Decoder (List ( String, String ))
decodeHeaders =
    Decode.list
        (Decode.map2 Tuple.pair
            (Decode.index 0 Decode.string)
            (Decode.index 1 Decode.string)
        )


reExecuteAllQueries : Data.Schema.SchemaMetadata -> Db.Db -> QueryManager.Model -> ( QueryManager.Model, List (Cmd Msg) )
reExecuteAllQueries schema db queryManager =
    Dict.foldl
        (\_ subscription ( accModel, accCmds ) ->
            let
                executionResult =
                    Db.executeQueryWithTracking schema db subscription.query

                resultJson =
                    encodeQueryResult executionResult.results

                nextRevision =
                    subscription.revision + 1

                updatedSubscription =
                    { subscription
                        | resultRowIds = executionResult.rowIds
                        , revision = nextRevision
                        , lastResult = Just executionResult.results
                    }

                updatedSubscriptions =
                    Dict.insert subscription.queryId updatedSubscription accModel.subscriptions

                updatedModel =
                    { accModel | subscriptions = updatedSubscriptions }
            in
            ( updatedModel
            , QueryManager.queryClientFull subscription.queryId nextRevision resultJson :: accCmds
            )
        )
        ( queryManager, [] )
        queryManager.subscriptions
