port module AcquireExample exposing (main)

import Db.Database
import Db.Edit
import Db.Edit.Note as Note
import Db.Edit.Reply as Reply
import Db.Id
import Db.Id.Acquire as Acquire
import Db.Id.Broker as Broker
import Json.Decode as Decode
import Json.Encode as Encode
import Platform


port pyreStoreOut : Encode.Value -> Cmd msg


port pyre_receiveAcquiredIds : (Decode.Value -> msg) -> Sub msg


port pyre_receiveMutationResult : (Decode.Value -> msg) -> Sub msg


port acquire : (Bool -> msg) -> Sub msg


port acquired : Encode.Value -> Cmd msg


port submit : (( String, String ) -> msg) -> Sub msg


port completed : Encode.Value -> Cmd msg


type alias Ids =
    { note : Db.Id.Note, reply : Maybe Db.Id.Reply }


type Msg
    = Acquire Bool
    | BrokerMsg Broker.Msg
    | Acquired (Result Acquire.Error Ids)
    | Problem Broker.Problem
    | Submit ( String, String )
    | Completed Decode.Value


config : Broker.Config Msg
config =
    { send = pyreStoreOut, toMsg = BrokerMsg, onProblem = Problem }


main : Program () { broker : Broker.Model Msg, ids : Maybe Ids } Msg
main =
    Platform.worker
        { init = \() -> ( { broker = Broker.init, ids = Nothing }, Cmd.none )
        , update = update
        , subscriptions = \_ -> Sub.batch [ acquire Acquire, submit Submit, pyre_receiveAcquiredIds (Broker.Received >> BrokerMsg), pyre_receiveMutationResult Completed ]
        }


update msg model =
    case msg of
        Acquire explicitReply ->
            let
                plan =
                    if explicitReply then
                        Acquire.start (\note reply -> Ids note (Just reply)) |> Acquire.id |> Acquire.id

                    else
                        Acquire.start (\note -> Ids note Nothing) |> Acquire.id

                ( broker, cmd ) =
                    Broker.send config (Acquire.request plan) Acquired model.broker
            in
            ( { model | broker = broker }, cmd )

        BrokerMsg event ->
            let
                ( broker, cmd ) =
                    Broker.update config event model.broker
            in
            ( { model | broker = broker }, cmd )

        Acquired (Ok ids) ->
            ( { model | ids = Just ids }
            , acquired (Encode.object [ ( "note", Db.Id.encodeUuid ids.note ), ( "reply", Maybe.map Db.Id.encodeUuid ids.reply |> Maybe.withDefault Encode.null ) ])
            )

        Acquired (Err _) ->
            ( { model | ids = Nothing }, acquired Encode.null )

        Problem _ ->
            ( model, acquired Encode.null )

        Submit ( instance, title ) ->
            case model.ids of
                Nothing ->
                    ( model, Cmd.none )

                Just ids ->
                    ( { model | ids = Nothing }
                    , pyreStoreOut
                        (Db.Edit.submit (Db.Database.fromString instance)
                            "related"
                            [ Note.create { id = "related", title = title } [ Note.withId ids.note ]
                            , Reply.create { noteKey = ids.note, title = title } (Maybe.map (Reply.withId >> List.singleton) ids.reply |> Maybe.withDefault [])
                            ]
                        )
                    )

        Completed wire ->
            let
                instance =
                    Decode.decodeValue (Decode.field "databaseId" Decode.string) wire |> Result.withDefault ""

                response =
                    Db.Edit.receive (Db.Database.fromString instance) "related" wire
                        |> Result.andThen
                            (\receipt ->
                                Result.map2 Tuple.pair (Note.createResult 0 receipt) (Reply.createResult 1 receipt)
                            )

                value =
                    case response of
                        Ok ( notes, replies ) ->
                            Encode.object
                                [ ( "notes", Encode.list (\row -> Db.Id.encodeUuid row.noteKey) notes.note )
                                , ( "replies", Encode.list (\row -> Db.Id.encodeUuid row.replyKey) replies.reply )
                                ]

                        Err error ->
                            Encode.object [ ( "error", Encode.string error ) ]
            in
            ( model, completed value )
