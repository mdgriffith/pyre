module Data.Catchup exposing (Model, Msg(..), ServerConfig, Status(..), UpdateResult, databaseEpoch, databaseId, init, pendingDatabaseEpoch, status, update)

import Data.Delta
import Data.IndexedDb
import Data.LiveSync as LiveSync
import Data.Value
import Db
import Dict exposing (Dict)
import Http
import Json.Decode as Decode
import Json.Encode as Encode
import String


type alias ServerConfig =
    { baseUrl : String
    , catchupPath : String
    , databaseId : Maybe String
    , headers : List ( String, String )
    , credentials : String
    , withCredentials : Bool
    }


type Status
    = NotStarted
    | Syncing LiveSync.SyncProgress
    | Synced
    | Error String


type alias SyncCursorEntry =
    { lastSeenUpdatedAt : Maybe Float
    , lastSeenPrimaryKey : Maybe Data.Value.Value
    , permissionHash : String
    , lastSeenDeleteSequence : Int
    }


type alias SyncCursor =
    Dict String SyncCursorEntry


type alias CatchupTableResult =
    { changes : List Change
    , permissionHash : String
    , lastSeenUpdatedAt : Maybe Float
    , lastSeenPrimaryKey : Maybe Data.Value.Value
    , lastSeenDeleteSequence : Int
    }


type Change
    = RowChange (Dict String Data.Value.Value)
    | DeleteChange Data.Value.Value


type CatchupResponse
    = CatchupPageReceived CatchupPage
    | DatabaseResetReceived DatabaseReset


type alias CatchupPage =
    { databaseId : Maybe String
    , databaseEpoch : String
    , serverRevision : Maybe Int
    , tables : Dict String CatchupTableResult
    , hasMore : Bool
    , snapshotTimestamp : Maybe Float
    }


type alias DatabaseReset =
    { databaseId : Maybe String
    , databaseEpoch : String
    }


type alias Model =
    { server : ServerConfig
    , status : Status
    , cursor : SyncCursor
    , databaseEpoch : Maybe String
    , pendingResetEpoch : Maybe String
    , initialDataLoaded : Bool
    , inProgress : Bool
    , tablesSynced : Int
    , pendingPage : Maybe CatchupPage
    , pendingWake : Bool
    , streamReady : Bool
    , buffered : List { epoch : String, revision : Int, source : String, delta : Data.Delta.Delta }
    , keyRevisions : Dict String Int
    , persistedRevision : Int
    , storingLive : Maybe String
    , restartFull : Bool
    , roundStart : Maybe SyncCursor
    , roundTimestamp : Maybe Float
    , persistedCursor : SyncCursor
    , pendingCursor : Maybe SyncCursor
    , drainRemaining : Int
    }


type Msg
    = InitialDataLoaded Data.IndexedDb.SyncCursor (Maybe String)
    | CatchupRequired
    | CatchupResponseReceived (Result Http.Error CatchupResponse)
    | DatabaseEpochResetCompleted String
    | DatabaseEpochResetFailed String String
    | PageStored String
    | PageFailed String
    | StreamConnected
    | StreamDisconnected String
    | LiveDelta String Int String Data.Delta.Delta


type alias UpdateResult =
    { model : Model
    , db : Db.Db
    , cmd : Cmd Msg
    , dbCmds : List (Cmd Db.Msg)
    , delta : Maybe Data.Delta.Delta
    , serverRevision : Maybe Int
    , touchedTables : List String
    , error : Maybe String
    , destructiveReset : Bool
    }


init : ServerConfig -> Model
init server =
    { server = server
    , status = NotStarted
    , cursor = Dict.empty
    , databaseEpoch = Nothing
    , pendingResetEpoch = Nothing
    , initialDataLoaded = False
    , inProgress = False
    , tablesSynced = 0
    , pendingPage = Nothing
    , pendingWake = False
    , streamReady = False
    , buffered = []
    , keyRevisions = Dict.empty
    , persistedRevision = 0
    , storingLive = Nothing
    , restartFull = False
    , roundStart = Nothing
    , roundTimestamp = Nothing
    , persistedCursor = Dict.empty
    , pendingCursor = Nothing
    , drainRemaining = 0
    }


status : Model -> Status
status model =
    model.status


databaseId : Model -> Maybe String
databaseId model =
    model.server.databaseId


databaseEpoch : Model -> Maybe String
databaseEpoch model =
    model.databaseEpoch


pendingDatabaseEpoch : Model -> Maybe String
pendingDatabaseEpoch model =
    model.pendingResetEpoch


update : Msg -> Model -> Db.Db -> UpdateResult
update msg model db =
    case msg of
        StreamConnected ->
            update CatchupRequired { model | streamReady = True } db

        StreamDisconnected message ->
            let
                result =
                    emptyUpdate { model | streamReady = False, status = Error message } db
            in
            { result | error = Just message }

        LiveDelta epoch revision source delta ->
            if model.databaseEpoch /= Nothing && model.databaseEpoch /= Just epoch then
                update CatchupRequired model db

            else if model.inProgress || not model.initialDataLoaded || not model.streamReady || model.databaseEpoch == Nothing || model.status /= Synced then
                if List.length model.buffered >= 5000 || List.sum (List.map (\buffered -> List.sum (List.map (.rows >> List.length) buffered.delta.tableGroups)) model.buffered) + List.sum (List.map (.rows >> List.length) delta.tableGroups) > 5000 then
                    -- Overflow restarts a full scan; never silently drop changes
                    -- and declare the handoff complete.
                    let
                        ( next, cmd ) =
                            startCatchupIfReady { model | buffered = [], restartFull = True, pendingWake = True }

                        result =
                            emptyUpdate next db
                    in
                    { result | cmd = cmd }

                else
                    let
                        ( next, cmd ) =
                            startCatchupIfReady { model | buffered = model.buffered ++ [ { epoch = epoch, revision = revision, source = source, delta = delta } ] }

                        result =
                            emptyUpdate next db
                    in
                    { result | cmd = cmd }

            else
                let
                    tables =
                        List.foldl
                            (\group acc ->
                                let
                                    entry =
                                        Dict.get group.tableName model.cursor
                                            |> Maybe.withDefault { lastSeenUpdatedAt = Nothing, lastSeenPrimaryKey = Nothing, permissionHash = "", lastSeenDeleteSequence = 0 }

                                    changes =
                                        if Data.Delta.isDeletion group then
                                            List.filterMap (List.head >> Maybe.map DeleteChange) group.rows

                                        else
                                            List.map (\row -> RowChange (List.map2 Tuple.pair group.headers row |> Dict.fromList)) group.rows

                                    previous =
                                        Dict.get group.tableName acc |> Maybe.map .changes |> Maybe.withDefault []
                                in
                                Dict.insert group.tableName { changes = previous ++ changes, permissionHash = entry.permissionHash, lastSeenUpdatedAt = entry.lastSeenUpdatedAt, lastSeenPrimaryKey = entry.lastSeenPrimaryKey, lastSeenDeleteSequence = entry.lastSeenDeleteSequence } acc
                            )
                            Dict.empty
                            delta.tableGroups
                in
                handleCatchupResponse (Ok (CatchupPageReceived { databaseId = model.server.databaseId, databaseEpoch = epoch, serverRevision = Just revision, tables = tables, hasMore = False, snapshotTimestamp = Nothing })) { model | storingLive = Just source } db

        InitialDataLoaded initialCursor storedEpoch ->
            let
                updatedCursor =
                    initialCursor

                baseModel =
                    { model | cursor = updatedCursor, persistedCursor = updatedCursor, databaseEpoch = storedEpoch, initialDataLoaded = True }

                ( nextModel, cmd ) =
                    startCatchupIfReady baseModel
            in
            { model = nextModel
            , db = db
            , cmd = cmd
            , dbCmds = []
            , delta = Nothing
            , serverRevision = Nothing
            , touchedTables = []
            , error = Nothing
            , destructiveReset = False
            }

        CatchupRequired ->
            let
                ( nextModel, cmd ) =
                    startCatchupIfReady { model | pendingWake = model.pendingWake || model.inProgress }
            in
            { model = nextModel
            , db = db
            , cmd = cmd
            , dbCmds = []
            , delta = Nothing
            , serverRevision = Nothing
            , touchedTables = []
            , error = Nothing
            , destructiveReset = False
            }

        CatchupResponseReceived result ->
            handleCatchupResponse result model db

        PageStored epoch ->
            case model.pendingPage of
                Just page ->
                    if page.databaseEpoch == epoch then
                        let
                            ( delta, updatedDb, _ ) =
                                applyCatchupDelta page db

                            next =
                                { model
                                    | cursor =
                                        if model.storingLive == Nothing then
                                            updateSyncCursor page model.cursor

                                        else
                                            model.cursor
                                    , persistedCursor = Maybe.withDefault model.persistedCursor model.pendingCursor
                                    , databaseEpoch = Just epoch
                                    , pendingPage = Nothing
                                    , pendingCursor = Nothing
                                    , inProgress = False
                                    , storingLive = Nothing
                                    , keyRevisions = rememberKeys page model.keyRevisions
                                    , persistedRevision = max model.persistedRevision (Maybe.withDefault 0 page.serverRevision)
                                    , drainRemaining =
                                        if model.storingLive == Nothing && not page.hasMore then
                                            List.length model.buffered

                                        else
                                            model.drainRemaining
                                }

                            ( final, cmd ) =
                                if not model.streamReady then
                                    ( { next | status = Error "Live stream disconnected" }, Cmd.none )

                                else if model.restartFull then
                                    startCatchupIfReady next

                                else if page.hasMore then
                                    ( { next | inProgress = True }, requestCatchup next.cursor next.server next.databaseEpoch )

                                else if not (List.isEmpty next.buffered) && (not next.pendingWake || next.drainRemaining > 0) then
                                    ( { next | status = Synced }, Cmd.none )

                                else if next.pendingWake then
                                    startCatchupIfReady next

                                else
                                    ( { next | status = Synced, roundStart = Nothing, roundTimestamp = Nothing }, Cmd.none )

                            result =
                                emptyUpdate final updatedDb
                        in
                        if not final.inProgress && final.streamReady then
                            case final.buffered of
                                buffered :: rest ->
                                    let
                                        drained =
                                            update (LiveDelta buffered.epoch buffered.revision buffered.source buffered.delta) { final | buffered = rest, drainRemaining = max 0 (final.drainRemaining - 1) } updatedDb
                                    in
                                    { drained | delta = delta, serverRevision = page.serverRevision, touchedTables = Dict.keys page.tables }

                                [] ->
                                    { result | cmd = cmd, delta = delta, serverRevision = page.serverRevision, touchedTables = Dict.keys page.tables }

                        else
                            { result | cmd = cmd, delta = delta, serverRevision = page.serverRevision, touchedTables = Dict.keys page.tables }

                    else
                        emptyUpdate model db

                Nothing ->
                    emptyUpdate model db

        PageFailed message ->
            failedUpdate message { model | pendingPage = Nothing } db

        DatabaseEpochResetCompleted completedEpoch ->
            if model.pendingResetEpoch == Just completedEpoch then
                let
                    nextModel =
                        { model
                            | databaseEpoch = Just completedEpoch
                            , pendingResetEpoch = Nothing
                            , cursor = Dict.empty
                            , inProgress = True
                        }
                in
                { model = nextModel
                , db = db
                , cmd = requestCatchup Dict.empty nextModel.server (Just completedEpoch)
                , dbCmds = []
                , delta = Nothing
                , serverRevision = Nothing
                , touchedTables = []
                , error = Nothing
                , destructiveReset = False
                }

            else
                emptyUpdate model db

        DatabaseEpochResetFailed failedEpoch message ->
            if model.pendingResetEpoch == Just failedEpoch then
                { model = { model | status = Error message, inProgress = False }
                , db = db
                , cmd = Cmd.none
                , dbCmds = []
                , delta = Nothing
                , serverRevision = Nothing
                , touchedTables = []
                , error = Just message
                , destructiveReset = False
                }

            else
                emptyUpdate model db


startCatchupIfReady : Model -> ( Model, Cmd Msg )
startCatchupIfReady model =
    case ( model.initialDataLoaded && model.streamReady, model.inProgress ) of
        ( True, False ) ->
            let
                boundary =
                    Dict.map
                        (\_ entry ->
                            { entry
                                | lastSeenPrimaryKey = Nothing
                                , lastSeenDeleteSequence = 0
                                , lastSeenUpdatedAt =
                                    if model.restartFull then
                                        Nothing

                                    else
                                        entry.lastSeenUpdatedAt
                            }
                        )
                        (if model.restartFull then
                            model.persistedCursor

                         else
                            Maybe.withDefault model.persistedCursor model.roundStart
                        )

                progress =
                    { table = Nothing
                    , tablesSynced = model.tablesSynced
                    , totalTables = Nothing
                    , complete = False
                    , error = Nothing
                    }
            in
            ( { model | cursor = boundary, roundStart = Just boundary, roundTimestamp = Nothing, inProgress = True, status = Syncing progress, pendingWake = False, storingLive = Nothing, restartFull = False, drainRemaining = 0 }
            , requestCatchup boundary model.server model.databaseEpoch
            )

        _ ->
            ( model, Cmd.none )


handleCatchupResponse : Result Http.Error CatchupResponse -> Model -> Db.Db -> UpdateResult
handleCatchupResponse result model db =
    case result of
        Ok (DatabaseResetReceived reset) ->
            case validateResponseDatabaseId model.server.databaseId reset.databaseId of
                Just message ->
                    failedUpdate message model db

                Nothing ->
                    { model =
                        { model
                            | status =
                                Syncing
                                    { table = Nothing
                                    , tablesSynced = 0
                                    , totalTables = Nothing
                                    , complete = False
                                    , error = Nothing
                                    }
                            , cursor = Dict.empty
                            , pendingResetEpoch = Just reset.databaseEpoch
                            , pendingPage = Nothing
                            , buffered = []
                            , keyRevisions = Dict.empty
                            , persistedRevision = 0
                            , persistedCursor = Dict.empty
                            , pendingCursor = Nothing
                            , roundStart = Just Dict.empty
                            , roundTimestamp = Nothing
                            , drainRemaining = 0
                            , restartFull = False
                            , inProgress = True
                            , tablesSynced = 0
                        }
                    , db = Db.init
                    , cmd = Cmd.none
                    , dbCmds = [ Data.IndexedDb.resetForDatabaseEpoch reset.databaseEpoch ]
                    , delta = Nothing
                    , serverRevision = Nothing
                    , touchedTables = []
                    , error = Nothing
                    , destructiveReset = True
                    }

        Ok (CatchupPageReceived unfiltered) ->
            let
                response =
                    { unfiltered
                        | tables =
                            Dict.map
                                (\name table ->
                                    { table
                                        | changes =
                                            List.filter
                                                (\change ->
                                                    not (model.storingLive /= Nothing && liveChangeCovered name change model)
                                                        && (Maybe.withDefault -1 (Dict.get (changeKey name change) model.keyRevisions)
                                                                < Maybe.withDefault 0 unfiltered.serverRevision
                                                                + (if model.storingLive /= Nothing then
                                                                    0

                                                                   else
                                                                    1
                                                                  )
                                                           )
                                                )
                                                table.changes
                                    }
                                )
                                unfiltered.tables
                    }
            in
            case validateResponseDatabaseId model.server.databaseId response.databaseId of
                Just message ->
                    { model = { model | status = Error message, inProgress = False }
                    , db = db
                    , cmd = Cmd.none
                    , dbCmds = []
                    , delta = Nothing
                    , serverRevision = response.serverRevision
                    , touchedTables = []
                    , error = Just message
                    , destructiveReset = False
                    }

                Nothing ->
                    let
                        tableGroups =
                            response.tables
                                |> Dict.toList
                                |> List.concatMap (\( name, table ) -> catchupChangesToGroups name table.changes)

                        updatedCursor =
                            updateSyncCursor response model.cursor

                        roundTimestamp =
                            case model.roundTimestamp of
                                Just timestamp ->
                                    Just timestamp

                                Nothing ->
                                    response.snapshotTimestamp

                        checkpoint =
                            if model.storingLive /= Nothing then
                                model.persistedCursor

                            else
                                Dict.map
                                    (\name entry ->
                                        let
                                            timestamp =
                                                case ( entry.lastSeenUpdatedAt, roundTimestamp ) of
                                                    ( Just scanned, Just started ) ->
                                                        Just (min scanned started)

                                                    _ ->
                                                        model.roundStart |> Maybe.andThen (Dict.get name) |> Maybe.andThen .lastSeenUpdatedAt
                                        in
                                        { entry
                                            | lastSeenUpdatedAt = timestamp
                                            , lastSeenPrimaryKey = Nothing
                                            , lastSeenDeleteSequence = max entry.lastSeenDeleteSequence (Dict.get name model.persistedCursor |> Maybe.map .lastSeenDeleteSequence |> Maybe.withDefault 0)
                                        }
                                    )
                                    updatedCursor

                        syncedCount =
                            model.tablesSynced + Dict.size response.tables

                        progress =
                            { table = Nothing
                            , tablesSynced = syncedCount
                            , totalTables = Nothing
                            , complete = not response.hasMore
                            , error = Nothing
                            }

                        nextStatus =
                            Syncing { progress | complete = False }

                        baseModel =
                            -- Rejected storage writes must not alter authoritative memory.
                            { model
                                | pendingPage = Just response
                                , pendingCursor = Just checkpoint
                                , roundTimestamp = roundTimestamp
                                , tablesSynced = syncedCount
                                , status = nextStatus
                                , inProgress = True
                            }

                        ( nextModel, cmd ) =
                            ( baseModel, Cmd.none )
                    in
                    { model = nextModel
                    , db = db
                    , cmd = cmd
                    , dbCmds =
                        [ Data.IndexedDb.writeCatchupPage response.databaseEpoch
                            (max (Maybe.withDefault 0 response.serverRevision) model.persistedRevision)
                            checkpoint
                            tableGroups
                            (Maybe.withDefault "catchup" model.storingLive)
                        ]
                    , delta = Nothing
                    , serverRevision = Nothing
                    , touchedTables = Dict.keys response.tables
                    , error = Nothing
                    , destructiveReset = False
                    }

        Err err ->
            let
                message =
                    httpErrorToString err
            in
            { model = { model | status = Error message, inProgress = False }
            , db = db
            , cmd = Cmd.none
            , dbCmds = []
            , delta = Nothing
            , serverRevision = Nothing
            , touchedTables = []
            , error = Just message
            , destructiveReset = False
            }


emptyUpdate : Model -> Db.Db -> UpdateResult
emptyUpdate model db =
    { model = model
    , db = db
    , cmd = Cmd.none
    , dbCmds = []
    , delta = Nothing
    , serverRevision = Nothing
    , touchedTables = []
    , error = Nothing
    , destructiveReset = False
    }


failedUpdate : String -> Model -> Db.Db -> UpdateResult
failedUpdate message model db =
    let
        result =
            emptyUpdate { model | status = Error message, inProgress = False } db
    in
    { result | error = Just message }


requestCatchup : SyncCursor -> ServerConfig -> Maybe String -> Cmd Msg
requestCatchup cursor server maybeEpoch =
    let
        url =
            server.baseUrl ++ server.catchupPath

        body =
            Http.jsonBody (encodeCatchupRequest cursor server maybeEpoch)
    in
    if includeCredentials server then
        Http.riskyRequest
            { method = "POST"
            , headers = httpHeaders server.headers
            , url = url
            , body = body
            , expect = Http.expectJson CatchupResponseReceived decodeCatchupResponse
            , timeout = Nothing
            , tracker = Nothing
            }

    else
        Http.request
            { method = "POST"
            , headers = httpHeaders server.headers
            , url = url
            , body = body
            , expect = Http.expectJson CatchupResponseReceived decodeCatchupResponse
            , timeout = Nothing
            , tracker = Nothing
            }


encodeCatchupRequest : SyncCursor -> ServerConfig -> Maybe String -> Encode.Value
encodeCatchupRequest cursor server maybeEpoch =
    Encode.object <|
        List.concat
            [ case server.databaseId of
                Just sourceDatabaseId ->
                    [ ( "databaseId", Encode.string sourceDatabaseId ) ]

                Nothing ->
                    []
            , case maybeEpoch of
                Just epoch ->
                    [ ( "databaseEpoch", Encode.string epoch ) ]

                Nothing ->
                    []
            , [ ( "syncCursor", encodeSyncCursor cursor ) ]
            ]


includeCredentials : ServerConfig -> Bool
includeCredentials server =
    server.credentials == "include" || server.withCredentials


httpHeaders : List ( String, String ) -> List Http.Header
httpHeaders headers =
    List.map (\( key, value ) -> Http.header key value) headers


applyCatchupDelta : CatchupPage -> Db.Db -> ( Maybe Data.Delta.Delta, Db.Db, List (Cmd Db.Msg) )
applyCatchupDelta response db =
    let
        tableGroups =
            response.tables
                |> Dict.toList
                |> List.concatMap
                    (\( tableName, tableResult ) ->
                        catchupChangesToGroups tableName tableResult.changes
                    )

        dbWithKnownTables =
            ensureTablesExist (Dict.keys response.tables) db
    in
    if List.isEmpty tableGroups then
        ( Nothing, dbWithKnownTables, [] )

    else
        let
            delta =
                { tableGroups = tableGroups }

            ( updatedDb, _ ) =
                Db.update (Db.LocalDeltaReceived delta) dbWithKnownTables
        in
        ( Just delta, updatedDb, [] )


ensureTablesExist : List String -> Db.Db -> Db.Db
ensureTablesExist tableNames db =
    let
        updatedTables =
            List.foldl
                (\tableName acc ->
                    case Dict.get tableName acc of
                        Just _ ->
                            acc

                        Nothing ->
                            Dict.insert tableName Dict.empty acc
                )
                db.tables
                tableNames
    in
    { db | tables = updatedTables }


catchupChangesToGroups : String -> List Change -> List Data.Delta.TableGroup
catchupChangesToGroups tableName changes =
    List.map
        (\change ->
            case change of
                DeleteChange key ->
                    Data.Delta.deletion tableName key

                RowChange row ->
                    { tableName = tableName, headers = Dict.keys row, rows = [ Dict.values row ] }
        )
        changes


updateSyncCursor : CatchupPage -> SyncCursor -> SyncCursor
updateSyncCursor response cursor =
    Dict.foldl
        (\tableName tableResult acc ->
            Dict.insert tableName
                { lastSeenUpdatedAt = tableResult.lastSeenUpdatedAt
                , lastSeenPrimaryKey = tableResult.lastSeenPrimaryKey
                , permissionHash = tableResult.permissionHash
                , lastSeenDeleteSequence = tableResult.lastSeenDeleteSequence
                }
                acc
        )
        cursor
        response.tables


computeSyncCursor : Db.Db -> SyncCursor -> SyncCursor
computeSyncCursor db cursor =
    let
        cursorWithMissingTablesReset =
            Dict.foldl
                (\tableName entry acc ->
                    case Dict.get tableName db.tables of
                        Nothing ->
                            resetCursorIfRowsAreMissing tableName entry acc

                        Just tableData ->
                            if Dict.isEmpty tableData then
                                resetCursorIfRowsAreMissing tableName entry acc

                            else
                                acc
                )
                cursor
                cursor
    in
    Dict.foldl
        (\tableName tableData acc ->
            let
                maxCursor =
                    computeMaxCursor tableData

                existingPermission =
                    Dict.get tableName cursor
                        |> Maybe.map .permissionHash
                        |> Maybe.withDefault ""

                updatedEntry =
                    { lastSeenUpdatedAt =
                        case maxCursor of
                            Just ( updatedAt, _ ) ->
                                Just updatedAt

                            Nothing ->
                                Dict.get tableName cursor
                                    |> Maybe.map .lastSeenUpdatedAt
                                    |> Maybe.withDefault Nothing
                    , lastSeenPrimaryKey =
                        case maxCursor of
                            Just ( _, primaryKey ) ->
                                Just primaryKey

                            Nothing ->
                                Dict.get tableName cursor
                                    |> Maybe.andThen .lastSeenPrimaryKey
                    , permissionHash = existingPermission
                    , lastSeenDeleteSequence = Dict.get tableName cursor |> Maybe.map .lastSeenDeleteSequence |> Maybe.withDefault 0
                    }
            in
            Dict.insert tableName updatedEntry acc
        )
        cursorWithMissingTablesReset
        db.tables


resetCursorIfRowsAreMissing : String -> SyncCursorEntry -> SyncCursor -> SyncCursor
resetCursorIfRowsAreMissing tableName entry cursor =
    case entry.lastSeenUpdatedAt of
        Just _ ->
            Dict.insert tableName
                { lastSeenUpdatedAt = Nothing
                , lastSeenPrimaryKey = Nothing
                , permissionHash = ""
                , lastSeenDeleteSequence = 0
                }
                cursor

        Nothing ->
            cursor


computeMaxCursor : Dict Int (Dict String Data.Value.Value) -> Maybe ( Float, Data.Value.Value )
computeMaxCursor tableData =
    Dict.toList tableData
        |> List.foldl updateMaxCursor Nothing


updateMaxCursor : ( Int, Dict String Data.Value.Value ) -> Maybe ( Float, Data.Value.Value ) -> Maybe ( Float, Data.Value.Value )
updateMaxCursor ( rowId, row ) currentMax =
    case Dict.get "updatedAt" row of
        Just value ->
            case valueToTimestamp value of
                Just timestamp ->
                    let
                        candidate =
                            ( timestamp, Data.Value.IntValue rowId )
                    in
                    case currentMax of
                        Just ( existingTimestamp, existingPrimaryKey ) ->
                            if timestamp > existingTimestamp || (timestamp == existingTimestamp && rowId > primaryKeyToInt existingPrimaryKey) then
                                Just candidate

                            else
                                currentMax

                        Nothing ->
                            Just candidate

                Nothing ->
                    currentMax

        Nothing ->
            currentMax


primaryKeyToInt : Data.Value.Value -> Int
primaryKeyToInt value =
    case value of
        Data.Value.IntValue primaryKey ->
            primaryKey

        _ ->
            -2147483648


valueToTimestamp : Data.Value.Value -> Maybe Float
valueToTimestamp value =
    case value of
        Data.Value.IntValue i ->
            Just (toFloat i)

        Data.Value.FloatValue f ->
            Just f

        Data.Value.StringValue str ->
            String.toFloat str

        _ ->
            Nothing


encodeSyncCursor : SyncCursor -> Encode.Value
encodeSyncCursor cursor =
    Encode.object
        [ ( "version", Encode.int 2 )
        , ( "tables", Encode.dict identity encodeSyncCursorEntry cursor )
        ]


encodeSyncCursorEntry : SyncCursorEntry -> Encode.Value
encodeSyncCursorEntry entry =
    Encode.object
        [ ( "last_seen_updated_at"
          , case entry.lastSeenUpdatedAt of
                Just value ->
                    Encode.float value

                Nothing ->
                    Encode.null
          )
        , ( "last_seen_primary_key"
          , entry.lastSeenPrimaryKey
                |> Maybe.map Data.Value.encodeValue
                |> Maybe.withDefault Encode.null
          )
        , ( "permission_hash", Encode.string entry.permissionHash )
        , ( "last_seen_delete_sequence", Encode.int entry.lastSeenDeleteSequence )
        ]


decodeCatchupResponse : Decode.Decoder CatchupResponse
decodeCatchupResponse =
    Decode.oneOf
        [ Decode.field "type" Decode.string
            |> Decode.andThen
                (\type_ ->
                    if type_ == "reset" then
                        Decode.map2 DatabaseReset
                            (Decode.maybe (Decode.field "databaseId" Decode.string))
                            (Decode.field "databaseEpoch" Decode.string)
                            |> Decode.map DatabaseResetReceived

                    else
                        Decode.fail ("Unknown catchup response type: " ++ type_)
                )
        , decodeCatchupPage |> Decode.map CatchupPageReceived
        ]


decodeCatchupPage : Decode.Decoder CatchupPage
decodeCatchupPage =
    Decode.field "syncVersion" Decode.int
        |> Decode.andThen
            (\version ->
                if version == 2 then
                    decodeDurablePage

                else
                    Decode.fail "Pyre sync protocol 2 required"
            )


decodeDurablePage : Decode.Decoder CatchupPage
decodeDurablePage =
    Decode.map6 CatchupPage
        (Decode.maybe (Decode.field "databaseId" Decode.string))
        (Decode.field "databaseEpoch" Decode.string)
        (Decode.map Just (Decode.field "serverRevision" Decode.int))
        (Decode.field "tables" (Decode.dict decodeCatchupTable))
        (Decode.field "has_more" Decode.bool)
        (Decode.maybe (Decode.field "snapshotTimestamp" Decode.float))


validateResponseDatabaseId : Maybe String -> Maybe String -> Maybe String
validateResponseDatabaseId expected actual =
    case expected of
        Nothing ->
            Nothing

        Just expectedId ->
            case actual of
                Just actualId ->
                    if actualId == expectedId then
                        Nothing

                    else
                        Just ("Catchup response databaseId mismatch: expected " ++ expectedId ++ ", got " ++ actualId)

                Nothing ->
                    Just ("Catchup response missing databaseId: expected " ++ expectedId)


decodeCatchupTable : Decode.Decoder CatchupTableResult
decodeCatchupTable =
    Decode.map5 CatchupTableResult
        (Decode.field "changes" (Decode.list decodeChange))
        (Decode.field "permission_hash" Decode.string)
        (Decode.field "last_seen_updated_at" decodeMaybeTimestamp)
        (Decode.maybe (Decode.field "last_seen_primary_key" Data.Value.decodeValue)
            |> Decode.map (Maybe.andThen valueToPrimaryKey)
        )
        (Decode.oneOf [ Decode.field "last_seen_delete_sequence" Decode.int, Decode.succeed 0 ])


changeKey : String -> Change -> String
changeKey table change =
    Encode.encode 0
        (Encode.list identity
            [ Encode.string table
            , case change of
                DeleteChange key ->
                    Data.Value.encodeValue key

                RowChange row ->
                    Dict.get "id" row |> Maybe.map Data.Value.encodeValue |> Maybe.withDefault Encode.null
            ]
        )


liveChangeCovered : String -> Change -> Model -> Bool
liveChangeCovered name change model =
    -- Only rows strictly BEFORE a safe persisted timestamp interval are
    -- certified by the cursor. Equal-second rows still require observations.
    case ( change, Dict.get (changeKey name change) model.keyRevisions ) of
        ( RowChange row, Nothing ) ->
            case ( Dict.get "updatedAt" row |> Maybe.andThen valueToTimestamp, Dict.get name model.persistedCursor |> Maybe.andThen .lastSeenUpdatedAt ) of
                ( Just timestamp, Just boundary ) ->
                    timestamp < boundary

                _ ->
                    False

        _ ->
            False


rememberKeys : CatchupPage -> Dict String Int -> Dict String Int
rememberKeys page revisions =
    Dict.foldl
        (\name table acc -> List.foldl (\change keys -> Dict.insert (changeKey name change) (Maybe.withDefault 0 page.serverRevision) keys) acc table.changes)
        revisions
        page.tables


decodeChange : Decode.Decoder Change
decodeChange =
    Decode.field "op" Decode.string
        |> Decode.andThen
            (\op ->
                case op of
                    "delete" ->
                        Decode.map DeleteChange (Decode.field "id" Data.Value.decodeValue)

                    "row" ->
                        Decode.map2 (\key row -> RowChange (Dict.insert "id" key row))
                            (Decode.field "id" Data.Value.decodeValue)
                            (Decode.field "row" (Decode.dict Data.Value.decodeValue))

                    _ ->
                        Decode.fail ("Unknown sync operation: " ++ op)
            )


valueToPrimaryKey : Data.Value.Value -> Maybe Data.Value.Value
valueToPrimaryKey value =
    case value of
        Data.Value.IntValue _ ->
            Just value

        Data.Value.StringValue _ ->
            Just value

        _ ->
            Nothing


decodeMaybeTimestamp : Decode.Decoder (Maybe Float)
decodeMaybeTimestamp =
    Decode.oneOf
        [ Decode.null Nothing
        , Decode.float |> Decode.map Just
        , Decode.int |> Decode.map (\value -> Just (toFloat value))
        ]


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
