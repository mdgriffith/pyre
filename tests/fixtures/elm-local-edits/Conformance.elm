port module Test exposing (main)

import Db.Database as Database
import Db.Default.Edit.Audit as Audit
import Db.Default.Edit.Command.NamedAudit as NamedAudit
import Db.Default.Edit.Issue as Issue
import Db.EditIds
import Db.Id
import Dict
import Json.Encode as E
import Pyre
import Pyre.Batch as Batch
import Pyre.LocalEdits as LocalEdits
import Time


port effectOut : E.Value -> Cmd msg


port incoming : (E.Value -> msg) -> Sub msg


port observed : E.Value -> Cmd msg


port perform : (String -> msg) -> Sub msg


port completed : E.Value -> Cmd msg


type Msg
    = Incoming E.Value
    | Perform String


type alias Model =
    { pyre : Pyre.Model
    , initialConfirmed : Bool
    , created : Maybe Db.EditIds.DefaultIssue
    , pending : Maybe ( String, LocalEdits.Receipt { id : Db.EditIds.DefaultIssue } )
    }


submission =
    Batch.succeed (\audit command -> ( audit, command ))
        |> Batch.and (Audit.create { message = "audit" })
        |> Batch.and (NamedAudit.run { id = Db.Id.uuid "00000000-0000-4000-8000-000000000024", message = "named" })
        |> (\plan -> Pyre.batch (Database.fromString "one") plan (Pyre.init "conformance"))


main : Program () Model Msg
main =
    Platform.worker
        { init =
            \_ ->
                let
                    ( model, effect, _ ) =
                        submission
                in
                ( { pyre = model, initialConfirmed = False, created = Nothing, pending = Nothing }
                , case effect of
                    Pyre.Send value ->
                        effectOut value

                    _ ->
                        Cmd.none
                )
        , update = update
        , subscriptions = \_ -> Sub.batch [ incoming Incoming, perform Perform ]
        }


update msg model =
    case msg of
        Incoming value ->
            let
                ( next, _ ) =
                    Pyre.update (Pyre.decodeIncomingDelta value) model.pyre

                ( _, _, receipt ) =
                    submission
            in
            if model.initialConfirmed then
                finish { model | pyre = next }

            else
                case Pyre.outcome receipt next of
                    Just (LocalEdits.Confirmed ( audit, command )) ->
                        ( { model | pyre = next, initialConfirmed = True }
                        , observed (E.object [ ( "auditId", Db.Id.encodeUuid audit.id ), ( "timestamps", E.list (E.int << Time.posixToMillis << .updatedAt) command.audit ) ])
                        )

                    _ ->
                        ( { model | pyre = next }, Cmd.none )

        Perform action ->
            let
                id =
                    model.created |> Maybe.withDefault (Db.Id.uuid "00000000-0000-4000-8000-000000000010")

                plan =
                    case action of
                        "create" ->
                            Batch.succeed identity |> Batch.and (Issue.create { title = "elm related", owner = "me" })

                        "nullable" ->
                            Batch.succeed identity |> Batch.and (Issue.update id [ Issue.setAssignee Nothing, Issue.setPayload (Just (Dict.fromList [ ( "items", [ Just 3, Nothing ] ) ])), Issue.setWatchers (Just [ "ABCDEFAB-CDEF-0123-4567-ABCDEFABCDEF" ]), Issue.setDueAt (Just (Time.millisToPosix 1767225600000)) ])

                        "delete" ->
                            Batch.succeed identity |> Batch.and (Issue.delete id)

                        "invalidUuid" ->
                            Batch.succeed identity |> Batch.and (Issue.update (Db.Id.uuid "00000000-0000-4000-8000-000000000010\n") [ Issue.setTitle "invalid" ])

                        "invalidStructured" ->
                            Batch.succeed identity |> Batch.and (Issue.update id [ Issue.setWatchers (Just [ "invalid" ]) ])

                        "emptyUpdate" ->
                            Batch.succeed identity |> Batch.and (Issue.update id [])

                        "rollback" ->
                            Batch.succeed (\_ last -> last)
                                |> Batch.and (Issue.create { title = "ghost", owner = "me" })
                                |> Batch.and (Issue.create { title = "denied", owner = "other" })

                        _ ->
                            Batch.succeed { id = id }

                ( next, effect, receipt ) =
                    Pyre.batch (Database.fromString "one") plan model.pyre

                pending =
                    { model | pyre = next, pending = Just ( action, receipt ) }
            in
            case effect of
                Pyre.Send value ->
                    ( pending, effectOut value )

                _ ->
                    finish pending


finish model =
    case model.pending of
        Just ( action, receipt ) ->
            case Pyre.outcome receipt model.pyre of
                Just outcome ->
                    let
                        state =
                            case outcome of
                                LocalEdits.Confirmed _ ->
                                    "confirmed"

                                LocalEdits.Rejected _ ->
                                    "rejected"

                                _ ->
                                    "unexpected"

                        outcomeFields =
                            case outcome of
                                LocalEdits.Rejected code ->
                                    [ ( "code", E.string code ) ]

                                _ ->
                                    []

                        created =
                            if action == "create" then
                                case outcome of
                                    LocalEdits.Confirmed result ->
                                        Just result.id

                                    _ ->
                                        model.created

                            else
                                model.created
                    in
                    ( { model | pending = Nothing, created = created }, completed (E.object ([ ( "action", E.string action ), ( "state", E.string state ) ] ++ outcomeFields)) )

                Nothing ->
                    ( model, Cmd.none )

        Nothing ->
            ( model, Cmd.none )
