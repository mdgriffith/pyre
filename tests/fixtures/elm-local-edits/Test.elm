port module Test exposing (main)

import Db
import Db.Archive.Edit.ArchiveEntry as Archive
import Db.Database as Database
import Db.Default.Edit.Audit as Audit
import Db.Default.Edit.Command.NamedAudit as NamedAudit
import Db.Default.Edit.Issue as Issue
import Db.EditIds
import Db.Id
import Dict
import Json.Decode as D
import Json.Encode as E
import Pyre
import Pyre.Batch as Batch
import Pyre.Edit exposing (Edit, Updated)
import Pyre.Edit.Internal as Internal
import Pyre.LocalEdits as LocalEdits
import Query.ReadIssues as Read
import Time


port output : Bool -> Cmd msg


port effectOut : E.Value -> Cmd msg


renameReadRow : Read.Issue -> Edit Database.Default (Updated Db.EditIds.DefaultIssue)
renameReadRow row =
    Issue.update row.id [ Issue.title row.title ]


port incoming : (E.Value -> msg) -> Sub msg


port settled : Bool -> Cmd msg


type Msg
    = Incoming E.Value


main : Program () Pyre.Model Msg
main =
    Platform.worker
        { init =
            \_ ->
                let
                    ( model, _, _ ) =
                        bridgeSubmission
                in
                ( model, Cmd.batch [ output checks, effectOut bridgeEffect ] )
        , update =
            \(Incoming value) model ->
                let
                    ( next, _ ) =
                        Pyre.update (Pyre.decodeIncomingDelta value) model

                    ( _, _, receipt ) =
                        bridgeSubmission
                in
                ( next
                , case Pyre.outcome receipt next of
                    Just (LocalEdits.Confirmed ( _, audit, commandResult )) ->
                        settled (audit.id == Db.Id.int 23 && List.map (.updatedAt >> Time.posixToMillis) commandResult.audit == [ 1700000000000 ])

                    _ ->
                        Cmd.none
                )
        , subscriptions = \_ -> incoming Incoming
        }


bridgeSubmission =
    let
        plan =
            Batch.succeed (\issue audit command -> ( issue, audit, command ))
                |> Batch.and (Issue.update (Db.Id.uuid "00000000-0000-4000-8000-000000000001") [ Issue.title "elm", Issue.assignee Nothing ])
                |> Batch.and (Audit.create { message = "audit" })
                |> Batch.and (NamedAudit.run { message = "named" })

    in
    Pyre.batch (Database.fromString "one") plan Pyre.init


bridgeEffect : E.Value
bridgeEffect =
    let
        ( _, effect, _ ) =
            bridgeSubmission
    in
    case effect of
        Pyre.Send value ->
            value

        _ ->
            E.null


checks : Bool
checks =
    let
        id =
            Db.Id.uuid "00000000-0000-4000-8000-000000000001"

        edit =
            Issue.update id [ Issue.title "first", Issue.assignee (Just id), Issue.title "last", Issue.assignee Nothing ]

        plan =
            Batch.succeed Tuple.pair
                |> Batch.and edit
                |> Batch.and (Audit.create { message = "created" })

        ( model, effect, receipt ) =
            Pyre.batch (Database.fromString "one") plan Pyre.init

        ( other, _, second ) =
            Pyre.submit (Database.fromString "two") (Issue.delete id) model

        ( empty, emptyEffect, emptyReceipt ) =
            Pyre.batch (Database.fromString "one") (Batch.succeed 42) other

        fields =
            Internal.operations (Internal.single edit) |> List.head |> Maybe.withDefault E.null

        effectSent =
            case effect of
                Pyre.Send _ ->
                    True

                _ ->
                    False

        values =
            E.list identity
                (Internal.operations plan
                    |> List.indexedMap
                        (\index operation ->
                            E.object
                                [ ( "index", E.int index )
                                , ( "operation", D.decodeValue (D.field "operation" D.value) operation |> Result.withDefault E.null )
                                , ( "value"
                                  , E.object
                                        [ ( "id"
                                          , if index == 0 then
                                                Db.Id.encodeUuid id

                                            else
                                                E.int 17
                                          )
                                        ]
                                  )
                                ]
                        )
                )

        lifecycle database status results =
            E.object [ ( "type", E.string "lifecycle" ), ( "databaseId", E.string database ), ( "requestId", E.string "elm:1" ), ( "state", E.string status ), ( "results", results ) ]

        accepted =
            Pyre.update (Pyre.decodeIncomingDelta (lifecycle "one" "accepted" values)) model |> Tuple.first

        confirmed =
            Pyre.update (Pyre.LocalEditReceived (lifecycle "one" "confirmed" values)) accepted |> Tuple.first

        foreign =
            Pyre.update (Pyre.LocalEditReceived (lifecycle "two" "confirmed" values)) model |> Tuple.first

        unknown =
            Pyre.update (Pyre.decodeIncomingDelta (lifecycle "one" "outcomeUnknown" E.null)) model |> Tuple.first

        late =
            Pyre.update (Pyre.decodeIncomingDelta (lifecycle "one" "confirmed" values)) unknown |> Tuple.first

        malformed =
            Pyre.update (Pyre.decodeIncomingDelta (lifecycle "one" "confirmed" (E.list identity []))) model |> Tuple.first

        failure =
            E.object [ ( "type", E.string "failure" ), ( "requestId", E.string "elm:1" ), ( "databaseId", E.string "one" ), ( "code", E.string "Denied" ), ( "certainty", E.string "rejected" ) ]

        failed =
            Pyre.update (Pyre.decodeIncomingDelta failure) model |> Tuple.first

        afterQuery =
            Pyre.update (Pyre.QueryUpdate (Pyre.ReadIssues (Database.fromString "one") "reader" {})) failed |> Tuple.first

        typed : Maybe (LocalEdits.Outcome ( { id : Db.EditIds.DefaultIssue }, { id : Db.EditIds.DefaultAudit } ))
        typed =
            Pyre.outcome receipt confirmed

        created =
            Issue.createWith { id = id, title = "new", owner = "me" } [ Issue.withAssignee Nothing ]
                |> Internal.single
                |> Internal.operations
                |> List.head
                |> Maybe.withDefault E.null

        command =
            NamedAudit.run { message = "named" } |> Internal.single

        archived =
            Archive.create { title = "other namespace" } |> Internal.single

        omitted =
            renameReadRow { id = id, title = "unchanged assignee" }
                |> Internal.single
                |> Internal.operations
                |> List.head
                |> Maybe.withDefault E.null

        setId =
            Issue.update id [ Issue.assignee (Just id) ]
                |> Internal.single
                |> Internal.operations
                |> List.head
                |> Maybe.withDefault E.null

        nested =
            Issue.update id [ Issue.payload (Just (Dict.fromList [ ( "items", [ Just 1, Nothing ] ) ])), Issue.choice (Just Db.Open) ]
                |> Internal.single
                |> Internal.operations
                |> List.head
                |> Maybe.withDefault E.null
    in
    effectSent
        && Pyre.editState receipt model
        == Just "queued"
        && Pyre.editState second other
        == Just "queued"
        && Pyre.outcome emptyReceipt empty
        == Just (LocalEdits.Confirmed 42)
        && (case emptyEffect of
                Pyre.NoEffect ->
                    True

                _ ->
                    False
           )
        && D.decodeValue (D.at [ "input", "title" ] D.string) fields
        == Ok "last"
        && D.decodeValue (D.at [ "input", "assignee" ] (D.nullable D.string)) fields
        == Ok Nothing
        && D.decodeValue (D.at [ "input", "id" ] D.string) created
        == Ok "00000000-0000-4000-8000-000000000001"
        && Pyre.outcome receipt accepted
        == Nothing
        && Pyre.outcome receipt foreign
        == Nothing
        && Pyre.outcome receipt malformed
        == Nothing
        && Pyre.outcome receipt unknown
        == Just (LocalEdits.OutcomeUnknown "")
        && Pyre.outcome receipt late
        == Pyre.outcome receipt confirmed
        && List.map .code (Pyre.failures failed)
        == [ "Denied" ]
        && Pyre.failures afterQuery
        == []
        && List.length (Internal.operations command)
        == 1
        && List.length (Internal.operations archived)
        == 1
        && D.decodeValue (D.at [ "input", "assignee" ] D.string) setId
        == Ok "00000000-0000-4000-8000-000000000001"
        && (D.decodeValue (D.at [ "input", "assignee" ] D.value) omitted |> Result.toMaybe)
        == Nothing
        && D.decodeValue (D.at [ "input", "payload", "items" ] (D.list (D.nullable D.int))) nested
        == Ok [ Just 1, Nothing ]
        && (case typed of
                Just (LocalEdits.Confirmed ( issue, audit )) ->
                    issue.id == id && audit.id == Db.Id.int 17

                _ ->
                    False
           )
