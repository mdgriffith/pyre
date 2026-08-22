port module Data.IndexedDb exposing
    ( Incoming(..)
    , InitialData
    , SyncCursor
    , receiveIncoming
    , requestInitialData
    , resetForDatabaseEpoch
    , writeDatabaseEpoch
    , writeDelta
    , writeDeltaWithEntityNotification
    , writeServerRevision
    , writeSyncCursor
    )

import Data.Delta exposing (TableGroup)
import Data.Value exposing (Value)
import Dict exposing (Dict)
import Json.Decode as Decode
import Json.Encode as Encode



-- IndexedDB-specific types


type alias InitialData =
    { tables : Dict String (List (Dict String Value))
    , cursor : SyncCursor
    , lastAppliedServerRevision : Maybe Int
    , databaseEpoch : Maybe String
    }


type alias SyncCursor =
    Dict String SyncCursorEntry


type alias SyncCursorEntry =
    { lastSeenUpdatedAt : Maybe Float
    , lastSeenPrimaryKey : Maybe Value
    , permissionHash : String
    }



-- tableName -> (id -> row)
-- IndexedDB Message Types


type Message
    = RequestInitialData
    | WriteDelta (List TableGroup)
    | WriteDeltaWithEntityNotification String (List TableGroup)
    | WriteSyncCursor SyncCursor
    | WriteServerRevision Int
    | WriteDatabaseEpoch String
    | ResetForDatabaseEpoch String


type Incoming
    = InitialDataReceived InitialData
    | DatabaseEpochResetCompleted String
    | DatabaseEpochResetFailed String String



-- Ports


port indexedDbOut : Encode.Value -> Cmd msg


port receiveIndexedDbMessage : (Decode.Value -> msg) -> Sub msg



-- Encoders


encodeMessage : Message -> Encode.Value
encodeMessage msg =
    case msg of
        RequestInitialData ->
            Encode.object
                [ ( "type", Encode.string "requestInitialData" )
                ]

        WriteDelta tableGroups ->
            Encode.object
                [ ( "type", Encode.string "writeDelta" )
                , ( "tableGroups", Encode.list Data.Delta.encodeTableGroup tableGroups )
                ]

        WriteDeltaWithEntityNotification source tableGroups ->
            Encode.object
                [ ( "type", Encode.string "writeDelta" )
                , ( "entityStreamSource", Encode.string source )
                , ( "tableGroups", Encode.list Data.Delta.encodeTableGroup tableGroups )
                ]

        WriteSyncCursor cursor ->
            Encode.object
                [ ( "type", Encode.string "writeSyncCursor" )
                , ( "cursor", encodeSyncCursor cursor )
                ]

        WriteServerRevision serverRevision ->
            Encode.object
                [ ( "type", Encode.string "writeServerRevision" )
                , ( "serverRevision", Encode.int serverRevision )
                ]

        WriteDatabaseEpoch databaseEpoch ->
            Encode.object
                [ ( "type", Encode.string "writeDatabaseEpoch" )
                , ( "databaseEpoch", Encode.string databaseEpoch )
                ]

        ResetForDatabaseEpoch databaseEpoch ->
            Encode.object
                [ ( "type", Encode.string "resetForDatabaseEpoch" )
                , ( "databaseEpoch", Encode.string databaseEpoch )
                ]



-- Decoders


decodeIncoming : Decode.Decoder Incoming
decodeIncoming =
    Decode.field "type" Decode.string
        |> Decode.andThen
            (\type_ ->
                case type_ of
                    "initialData" ->
                        Decode.field "data" decodeInitialData
                            |> Decode.map InitialDataReceived

                    "databaseEpochResetCompleted" ->
                        Decode.field "databaseEpoch" Decode.string
                            |> Decode.map DatabaseEpochResetCompleted

                    "databaseEpochResetFailed" ->
                        Decode.map2 DatabaseEpochResetFailed
                            (Decode.field "databaseEpoch" Decode.string)
                            (Decode.field "error" Decode.string)

                    _ ->
                        Decode.fail ("Unknown IndexedDB incoming type: " ++ type_)
            )


decodeInitialData : Decode.Decoder InitialData
decodeInitialData =
    Decode.map4 InitialData
        (Decode.field "tables" (Decode.dict (Decode.list (Decode.dict Data.Value.decodeValue))))
        (Decode.field "cursor" decodeSyncCursor)
        (Decode.maybe (Decode.field "lastAppliedServerRevision" Decode.int))
        (Decode.maybe (Decode.field "databaseEpoch" Decode.string))


decodeSyncCursor : Decode.Decoder SyncCursor
decodeSyncCursor =
    Decode.field "tables" (Decode.dict decodeSyncCursorEntry)


decodeSyncCursorEntry : Decode.Decoder SyncCursorEntry
decodeSyncCursorEntry =
    Decode.map3 SyncCursorEntry
        (Decode.field "last_seen_updated_at" decodeMaybeTimestamp)
        (Decode.maybe (Decode.field "last_seen_primary_key" Data.Value.decodeValue)
            |> Decode.map (Maybe.andThen valueToPrimaryKey)
        )
        (Decode.field "permission_hash" Decode.string)


decodeMaybeTimestamp : Decode.Decoder (Maybe Float)
decodeMaybeTimestamp =
    Decode.oneOf
        [ Decode.null Nothing
        , Decode.float |> Decode.map Just
        , Decode.int |> Decode.map (toFloat >> Just)
        ]


encodeSyncCursor : SyncCursor -> Encode.Value
encodeSyncCursor cursor =
    Encode.object
        [ ( "tables", Encode.dict identity encodeSyncCursorEntry cursor ) ]


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
        ]


valueToPrimaryKey : Value -> Maybe Value
valueToPrimaryKey value =
    case value of
        Data.Value.IntValue _ ->
            Just value

        Data.Value.StringValue _ ->
            Just value

        _ ->
            Nothing



-- Helper functions


sendMessage : Message -> Cmd msg
sendMessage msg =
    indexedDbOut (encodeMessage msg)


requestInitialData : Cmd msg
requestInitialData =
    sendMessage RequestInitialData


writeDelta : List TableGroup -> Cmd msg
writeDelta tableGroups =
    sendMessage (WriteDelta tableGroups)


writeDeltaWithEntityNotification : String -> List TableGroup -> Cmd msg
writeDeltaWithEntityNotification source tableGroups =
    sendMessage (WriteDeltaWithEntityNotification source tableGroups)


writeSyncCursor : SyncCursor -> Cmd msg
writeSyncCursor cursor =
    sendMessage (WriteSyncCursor cursor)


writeServerRevision : Int -> Cmd msg
writeServerRevision serverRevision =
    sendMessage (WriteServerRevision serverRevision)


writeDatabaseEpoch : String -> Cmd msg
writeDatabaseEpoch databaseEpoch =
    sendMessage (WriteDatabaseEpoch databaseEpoch)


resetForDatabaseEpoch : String -> Cmd msg
resetForDatabaseEpoch databaseEpoch =
    sendMessage (ResetForDatabaseEpoch databaseEpoch)


receiveIncoming : (Result Decode.Error Incoming -> msg) -> Sub msg
receiveIncoming toMsg =
    receiveIndexedDbMessage (\jsonValue -> toMsg (Decode.decodeValue decodeIncoming jsonValue))
