module Db.Id.Broker exposing
    ( Action(..)
    , Config
    , Model
    , Msg(..)
    , Problem(..)
    , init
    , perform
    , receive
    , replaceBridge
    , reset
    , send
    , start
    , timeout
    , update
    )

{-| Keep one broker in the root model for the lifetime of the Elm app. Return
the new model and the command together so the handler is registered before
the port can reply. Use `reset` (not `init`) when replacing a bridge.
-}

import Db.Id.Acquire as Acquire
import Dict exposing (Dict)
import Json.Decode as Decode
import Json.Encode as Encode
import Process
import Task


type Model msg
    = Model Int (Dict String (Handler msg))


type alias Handler msg =
    { reply : Decode.Value -> msg
    , fail : Acquire.Error -> msg
    }


type Msg
    = Received Decode.Value
    | Timeout String


type Problem
    = MalformedResponseId String
    | UnknownResponseId String


type Action msg
    = Send Encode.Value
    | Wait String Float
    | Deliver msg
    | Report Problem


type alias Config msg =
    { send : Encode.Value -> Cmd msg
    , toMsg : Msg -> msg
    , onProblem : Problem -> msg
    }


init : Model msg
init =
    Model 0 Dict.empty


start : Acquire.Request a -> (Result Acquire.Error a -> msg) -> Model msg -> ( Model msg, List (Action msg) )
start acquisition toMessage ((Model serial pending) as model) =
    case acquisition of
        Acquire.Ready value ->
            ( model, [ Deliver (toMessage (Ok value)) ] )

        Acquire.Failed error ->
            ( model, [ Deliver (toMessage (Err error)) ] )

        Acquire.NeedsIds plan ->
            if serial >= 2147483647 then
                ( model, [ Deliver (toMessage (Err Acquire.RequestIdsExhausted)) ] )

            else
                let
                    key =
                        "pyre:" ++ String.fromInt serial
                in
                case Acquire.describe key plan of
                    Err error ->
                        ( model, [ Deliver (toMessage (Err error)) ] )

                    Ok description ->
                        let
                            handler =
                                { reply =
                                    \wire ->
                                        Decode.decodeValue (Acquire.responseDecoder key plan) wire
                                            |> Result.mapError (Decode.errorToString >> Acquire.InvalidResponse)
                                            |> Result.andThen identity
                                            |> toMessage
                                , fail = Err >> toMessage
                                }
                        in
                        ( Model (serial + 1) (Dict.insert key handler pending)
                        , [ Send (Acquire.encode description), Wait key 5000 ]
                        )


receive : Decode.Value -> Model msg -> ( Model msg, List (Action msg) )
receive wire ((Model serial pending) as model) =
    case Decode.decodeValue Acquire.responseId wire of
        Err error ->
            ( model, [ Report (MalformedResponseId (Decode.errorToString error)) ] )

        Ok key ->
            case Dict.get key pending of
                Nothing ->
                    ( model, [ Report (UnknownResponseId key) ] )

                Just handler ->
                    ( Model serial (Dict.remove key pending), [ Deliver (handler.reply wire) ] )


timeout : String -> Model msg -> ( Model msg, List (Action msg) )
timeout key ((Model serial pending) as model) =
    case Dict.get key pending of
        Nothing ->
            ( model, [] )

        Just handler ->
            ( Model serial (Dict.remove key pending), [ Deliver (handler.fail Acquire.TimedOut) ] )


{-| Clear pending handlers while keeping correlation IDs unique across bridge
lifetimes. Already scheduled timeouts become no-ops.
-}
replaceBridge : Model msg -> ( Model msg, List (Action msg) )
replaceBridge (Model serial pending) =
    ( Model serial Dict.empty
    , List.map (\handler -> Deliver (handler.fail Acquire.BridgeReplaced)) (Dict.values pending)
    )


perform : Config msg -> ( Model msg, List (Action msg) ) -> ( Model msg, Cmd msg )
perform config ( model, actions ) =
    ( model
    , Cmd.batch
        (List.map
            (\action ->
                case action of
                    Send wire ->
                        config.send wire

                    Wait key milliseconds ->
                        Process.sleep milliseconds |> Task.perform (\_ -> config.toMsg (Timeout key))

                    Deliver message ->
                        Task.succeed message |> Task.perform identity

                    Report problem ->
                        Task.succeed (config.onProblem problem) |> Task.perform identity
            )
            actions
        )
    )


send : Config msg -> Acquire.Request a -> (Result Acquire.Error a -> msg) -> Model msg -> ( Model msg, Cmd msg )
send config acquisition toMessage model =
    perform config (start acquisition toMessage model)


update : Config msg -> Msg -> Model msg -> ( Model msg, Cmd msg )
update config message model =
    perform config
        (case message of
            Received wire ->
                receive wire model

            Timeout key ->
                timeout key model
        )


reset : Config msg -> Model msg -> ( Model msg, Cmd msg )
reset config model =
    perform config (replaceBridge model)
