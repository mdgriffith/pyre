port module Data.LiveSync exposing
    ( Config
    , Incoming(..)
    , Message
    , SyncProgress
    , Transport(..)
    , connect
    , decodeConfig
    , receiveIncoming
    )

import Data.Delta exposing (Delta)
import Json.Decode as Decode
import Json.Encode as Encode


type Transport
    = Sse
    | WebSocket


type alias Config =
    { transport : Transport
    }


type alias SyncProgress =
    { table : Maybe String
    , tablesSynced : Int
    , totalTables : Maybe Int
    , complete : Bool
    , error : Maybe String
    }


type Message
    = ConnectSse
    | DisconnectSse
    | ConnectWebSocket
    | DisconnectWebSocket


type Incoming
    = DeltaReceived (Maybe String) (Maybe String) (Maybe Int) Delta
    | SyncProgressReceived (Maybe String) SyncProgress
    | LiveSyncConnected (Maybe String) (Maybe String) String
    | LiveSyncError String
    | SyncCompleteReceived (Maybe String)
    | SyncRequiredReceived (Maybe String) (Maybe String) (Maybe Int)


port sseOut : Encode.Value -> Cmd msg


port receiveSSEMessage : (Decode.Value -> msg) -> Sub msg


port webSocketOut : Encode.Value -> Cmd msg


port receiveWebSocketMessage : (Decode.Value -> msg) -> Sub msg


encodeMessage : Message -> Encode.Value
encodeMessage msg =
    case msg of
        ConnectSse ->
            Encode.object
                [ ( "type", Encode.string "connectSSE" ) ]

        DisconnectSse ->
            Encode.object
                [ ( "type", Encode.string "disconnectSSE" ) ]

        ConnectWebSocket ->
            Encode.object
                [ ( "type", Encode.string "connectWebSocket" ) ]

        DisconnectWebSocket ->
            Encode.object
                [ ( "type", Encode.string "disconnectWebSocket" ) ]


encodeConfig : Config -> Encode.Value
encodeConfig config =
    Encode.object
        [ ( "transport", encodeTransport config.transport ) ]


encodeTransport : Transport -> Encode.Value
encodeTransport transport =
    case transport of
        Sse ->
            Encode.string "sse"

        WebSocket ->
            Encode.string "websocket"


decodeIncoming : Decode.Decoder Incoming
decodeIncoming =
    Decode.field "type" Decode.string
        |> Decode.andThen
            (\type_ ->
                case type_ of
                    "delta" ->
                        Decode.map4 DeltaReceived
                            (Decode.maybe (Decode.field "databaseId" Decode.string))
                            (Decode.maybe (Decode.field "databaseEpoch" Decode.string))
                            (Decode.maybe (Decode.field "serverRevision" Decode.int))
                            (Decode.field "data" Data.Delta.decodeDelta)

                    "syncProgress" ->
                        Decode.map2 SyncProgressReceived
                            (Decode.maybe (Decode.field "databaseId" Decode.string))
                            (Decode.field "data" decodeSyncProgress)

                    "connected" ->
                        Decode.map3 LiveSyncConnected
                            (Decode.maybe (Decode.field "databaseId" Decode.string))
                            (Decode.maybe (Decode.field "databaseEpoch" Decode.string))
                            (Decode.field "connectionId" Decode.string)

                    "error" ->
                        Decode.field "error" Decode.string
                            |> Decode.map LiveSyncError

                    "syncComplete" ->
                        Decode.maybe (Decode.field "databaseId" Decode.string)
                            |> Decode.map SyncCompleteReceived

                    "syncRequired" ->
                        Decode.map3 SyncRequiredReceived
                            (Decode.maybe (Decode.field "databaseId" Decode.string))
                            (Decode.maybe (Decode.field "databaseEpoch" Decode.string))
                            (Decode.maybe (Decode.field "serverRevision" Decode.int))

                    "catchupRequired" ->
                        Decode.map3 SyncRequiredReceived
                            (Decode.maybe (Decode.field "databaseId" Decode.string))
                            (Decode.maybe (Decode.field "databaseEpoch" Decode.string))
                            (Decode.maybe (Decode.field "serverRevision" Decode.int))

                    _ ->
                        Decode.fail ("Unknown live sync incoming type: " ++ type_)
            )


decodeConfig : Decode.Decoder Config
decodeConfig =
    Decode.map Config
        (Decode.field "transport" decodeTransport)


decodeTransport : Decode.Decoder Transport
decodeTransport =
    Decode.string
        |> Decode.andThen
            (\value ->
                case value of
                    "sse" ->
                        Decode.succeed Sse

                    "websocket" ->
                        Decode.succeed WebSocket

                    _ ->
                        Decode.fail ("Unknown live sync transport: " ++ value)
            )


decodeSyncProgress : Decode.Decoder SyncProgress
decodeSyncProgress =
    Decode.map5 SyncProgress
        (Decode.maybe (Decode.field "table" Decode.string))
        (Decode.field "tablesSynced" Decode.int)
        (Decode.maybe (Decode.field "totalTables" Decode.int))
        (Decode.field "complete" Decode.bool)
        (Decode.maybe (Decode.field "error" Decode.string))


sendSseMessage : Message -> Cmd msg
sendSseMessage msg =
    sseOut (encodeMessage msg)


sendWebSocketMessage : Message -> Cmd msg
sendWebSocketMessage msg =
    webSocketOut (encodeMessage msg)


receiveIncoming : (Result Decode.Error Incoming -> msg) -> Sub msg
receiveIncoming toMsg =
    Sub.batch
        [ receiveSSEMessage (\jsonValue -> toMsg (Decode.decodeValue decodeIncoming jsonValue))
        , receiveWebSocketMessage (\jsonValue -> toMsg (Decode.decodeValue decodeIncoming jsonValue))
        ]


connect : Config -> Cmd msg
connect config =
    case config.transport of
        Sse ->
            sendSseMessage ConnectSse

        WebSocket ->
            sendWebSocketMessage ConnectWebSocket
