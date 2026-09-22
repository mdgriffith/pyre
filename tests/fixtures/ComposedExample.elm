port module ComposedExample exposing (main)

import Db.Database
import Db.Edit
import Db.Edit.Note as Note
import Db.Id
import Json.Decode as Decode
import Json.Encode as Encode
import Platform
import Pyre


port pyreStoreOut : Encode.Value -> Cmd msg
port pyre_receiveMutationResult : (Decode.Value -> msg) -> Sub msg
port createNote : (String -> msg) -> Sub msg
port updateNote : (( String, String ) -> msg) -> Sub msg
port completed : Encode.Value -> Cmd msg
port pyre_receiveQueryDelta : (Decode.Value -> msg) -> Sub msg
port observe : (() -> msg) -> Sub msg
port queryCounts : Encode.Value -> Cmd msg


type Msg
    = Create String
    | Update ( String, String )
    | Received Decode.Value
    | QueryReceived Decode.Value
    | Observe


main =
    Platform.worker
        { init = \() -> ( Pyre.init, Cmd.none )
        , update = update
        , subscriptions = \_ -> Sub.batch [ createNote Create, updateNote Update, pyre_receiveMutationResult Received, pyre_receiveQueryDelta QueryReceived, observe (\_ -> Observe) ]
        }


update msg model =
    case msg of
        Observe ->
            let
                ( first, firstEffect ) = Pyre.update (Pyre.QueryUpdate (Pyre.Notes (Db.Database.fromString "first") "same-query" {})) model
                ( second, secondEffect ) = Pyre.update (Pyre.QueryUpdate (Pyre.Notes (Db.Database.fromString "second") "same-query" {})) first
            in
            ( second, Cmd.batch [ effect firstEffect, effect secondEffect ] )

        QueryReceived wire ->
            let
                ( next, command ) = Pyre.update (Pyre.decodeIncomingDelta wire) model
                count instance = Pyre.getResult (Db.Database.fromString instance) "same-query" next.notes |> Maybe.map (\value -> List.length value.note) |> Maybe.withDefault -1
            in
            ( next, Cmd.batch [ effect command, queryCounts (Encode.object [ ( "first", Encode.int (count "first") ), ( "second", Encode.int (count "second") ) ]) ] )

        Create instance ->
            ( model
            , pyreStoreOut
                (Db.Edit.submit (Db.Database.fromString instance) "create-note"
                    [ Note.create { id = "ordinary", title = "Elm optimistic" } [] ]
                )
            )
        Update ( instance, key ) ->
            ( model, pyreStoreOut (Db.Edit.submit (Db.Database.fromString instance) "update-note" [ Note.update (Db.Id.uuid key) [ Note.title "Elm optimistic update" ] ]) )

        Received wire ->
            let
                instance =
                    Decode.decodeValue (Decode.field "databaseId" Decode.string) wire |> Result.withDefault ""

                request =
                    Decode.decodeValue (Decode.field "requestId" Decode.string) wire |> Result.withDefault ""

                count =
                    if request == "create-note" then
                        Db.Edit.receive (Db.Database.fromString instance) request wire
                            |> Result.andThen (Note.createResult 0)
                            |> Result.map (\value -> List.length value.note)

                    else
                        Db.Edit.receive (Db.Database.fromString instance) request wire
                            |> Result.andThen (Note.updateResult 0)
                            |> Result.map (\value -> List.length value.note)
            in
            ( model
            , completed
                (Encode.object
                    [ ( "databaseId", Encode.string instance )
                    , ( "ok", Encode.bool (Result.toMaybe count /= Nothing) )
                    , ( "count", Encode.int (Result.withDefault 0 count) )
                    ]
                )
            )


effect command =
    case command of
        Pyre.Send value -> pyreStoreOut value
        _ -> Cmd.none
