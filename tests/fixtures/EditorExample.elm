port module EditorExample exposing (main)

import Browser
import Db
import Db.Database
import Db.Edit
import Db.Edit.Account.Draft as AccountDraft
import Db.Edit.Account.Editor as AccountEditor
import Db.Edit.Document.Draft as Document
import Db.Edit.Document.Editor as DocumentEditor
import Db.Editor as Editor
import Db.Editor.Field as Field
import Db.Editor.Input.Default as Input
import Db.Id
import Dict
import Html exposing (Html)
import Html.Attributes as Attr
import Html.Events as Events
import Json.Decode as Decode
import Json.Encode as Encode
import Pyre


port editorReport : Encode.Value -> Cmd msg


port editorAction : (String -> msg) -> Sub msg


port pyreStoreOut : Encode.Value -> Cmd msg


port pyre_receiveQueryDelta : (Decode.Value -> msg) -> Sub msg


port pyre_receiveMutationResult : (Decode.Value -> msg) -> Sub msg



-- The widget needs only the view, the namespace's string constructor, and the form.


textInput : String -> Field.View fieldId Input.Kind Input.Value -> Editor.Form fieldId Input.Kind Input.Value result -> (Editor.Form fieldId Input.Kind Input.Value result -> msg) -> Html msg
textInput label field form toMsg =
    case field.kind of
        Input.StringKind ->
            let
                text =
                    case field.current of
                        Db.Present (Input.StringValue current) ->
                            current

                        _ ->
                            ""
            in
            Html.label []
                [ Html.text label
                , Html.input
                    [ Attr.value text
                    , Events.onInput (\next -> toMsg (Editor.set field.id (Db.Present (Input.StringValue next)) form))
                    ]
                    []
                ]

        _ ->
            Html.text ""


widget label id form toMsg =
    case List.filter (\field -> field.id == id) (Editor.fields form) of
        field :: _ ->
            textInput label field form toMsg

        [] ->
            Html.text ""


type alias Model =
    { draft : Maybe Document.Draft
    , creating : Editor.Form DocumentEditor.FieldId Input.Kind Input.Value Document.Draft
    , account : Editor.Form AccountEditor.FieldId Input.Kind Input.Value AccountDraft.Draft
    , queries : Pyre.Model
    , editing : Maybe ( Db.Id.Document, Editor.Form DocumentEditor.FieldId Input.Kind Input.Value Document.Patch )
    }


type Msg
    = Action String
    | QueryDelta Decode.Value
    | MutationResult Decode.Value
    | DocumentChanged (Editor.Form DocumentEditor.FieldId Input.Kind Input.Value Document.Draft)
    | AccountChanged (Editor.Form AccountEditor.FieldId Input.Kind Input.Value AccountDraft.Draft)


database =
    Db.Database.fromString "first"


requestId =
    "editor-doc"


main : Program () Model Msg
main =
    Browser.element
        { init = \() -> ( { draft = Nothing, creating = DocumentEditor.createForm, account = AccountEditor.createForm, queries = Pyre.init, editing = Nothing }, Cmd.none )
        , update = update
        , view = view
        , subscriptions = \_ -> Sub.batch [ editorAction Action, pyre_receiveQueryDelta QueryDelta, pyre_receiveMutationResult MutationResult ]
        }


view : Model -> Html Msg
view model =
    Html.div []
        [ widget "Title" DocumentEditor.Title model.creating DocumentChanged
        , widget "Owner" DocumentEditor.Owner model.creating DocumentChanged
        , widget "Account name" AccountEditor.Name model.account AccountChanged
        ]


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        DocumentChanged creating ->
            ( { model | creating = creating }, Cmd.none )

        AccountChanged account ->
            ( { model | account = account }, Cmd.none )

        Action "check-create" ->
            let
                ( invalid, empty ) =
                    Editor.validate DocumentEditor.createForm

                ( wrong, checked ) =
                    empty
                        |> Editor.set DocumentEditor.Title (Db.Present (Input.BoolValue True))
                        |> Editor.set DocumentEditor.Tags Db.Null
                        |> Editor.validate

                repaired =
                    Editor.set DocumentEditor.Title (Db.Present (Input.StringValue "Draft title")) checked

                ( draft, _ ) =
                    model.creating
                        |> Editor.set DocumentEditor.Tags (Db.Present (Input.StringListValue [ "draft" ]))
                        |> Editor.set DocumentEditor.Summary Db.Null
                        |> Editor.set DocumentEditor.Status (Db.Present (Input.Nullable_Value (Db.Present_ { reason = "Done", details = Nothing })))
                        |> Editor.set DocumentEditor.Groups (Db.Present (Input.Nullable_MaybeListDictValue (Dict.singleton "draft" [ Just Db.Absent_, Nothing ])))
                        |> Editor.set DocumentEditor.Labels (Db.Present (Input.StringList_Value Db.EmptyStrings))
                        |> Editor.validate

                ( account, _ ) =
                    Editor.validate model.account

                initialViews =
                    Editor.fields DocumentEditor.createForm
            in
            ( { model | draft = draft }
            , report "create"
                (invalid
                    == Nothing
                    && List.all (\field -> field.current == Db.Absent && field.original == Db.Absent) initialViews
                    && List.any (\field -> field.id == DocumentEditor.Status && field.choices == [ "Absent", "Null", "Present" ]) initialViews
                    && List.member ( DocumentEditor.Title, Field.Required ) (Editor.errors empty)
                    && wrong
                    == Nothing
                    && List.member ( DocumentEditor.Title, Field.WrongKind ) (Editor.errors checked)
                    && List.member ( DocumentEditor.Tags, Field.NotNullable ) (Editor.errors checked)
                    && not (List.any (Tuple.first >> (==) DocumentEditor.Title) (Editor.errors repaired))
                    && Maybe.map (\value -> ( value.title, value.owner )) draft
                    == Just ( "Draft title", "Owner" )
                    && Maybe.map .name account
                    == Just "Account"
                )
            )

        Action "submit-create" ->
            ( model
            , model.draft
                |> Maybe.map (\draft -> pyreStoreOut (Db.Edit.submit database "editor-create" [ Document.create draft ]))
                |> Maybe.withDefault Cmd.none
            )

        Action "open" ->
            case syncedRow model of
                Just row ->
                    let
                        form =
                            DocumentEditor.editForm row

                        ( bad, unavailable ) =
                            form |> Editor.set DocumentEditor.Owner (Db.Present (Input.StringValue "Immutable")) |> Editor.validate

                        equivalent =
                            json "{\"a\":[1,{}],\"hasOwnProperty\":true}"

                        ( same, _ ) =
                            form |> Editor.set DocumentEditor.Payload (Db.Present (Input.JsonValue equivalent)) |> Editor.validate

                        ( sameKeys, _ ) =
                            DocumentEditor.editForm { row | payload = Just (json "{\"__proto__\":{},\"hasOwnProperty\":true}") }
                                |> Editor.set DocumentEditor.Payload (Db.Present (Input.JsonValue (json "{\"hasOwnProperty\":true,\"__proto__\":{}}")))
                                |> Editor.validate

                        ( sameGroups, _ ) =
                            DocumentEditor.editForm { row | groups = Just (groups "{\"a\":0,\"hasOwnProperty\":true}") }
                                |> Editor.set DocumentEditor.Groups (Db.Present (Input.Nullable_MaybeListDictValue (groups "{\"hasOwnProperty\":true,\"a\":0}")))
                                |> Editor.validate

                        ( arrayChange, _ ) =
                            DocumentEditor.editForm { row | payload = Just (json "{}") }
                                |> Editor.set DocumentEditor.Payload (Db.Present (Input.JsonValue (json "[]")))
                                |> Editor.validate

                        edited =
                            unavailable
                                |> Editor.set DocumentEditor.Title (Db.Present (Input.StringValue "Local edit"))
                                |> Editor.set DocumentEditor.Title Db.Absent
                                |> Editor.set DocumentEditor.Tags (Db.Present (Input.StringListValue [ "whole", "replacement" ]))
                                |> Editor.set DocumentEditor.Caption Db.Null
                                |> Editor.set DocumentEditor.Summary Db.Null
                                |> Editor.set DocumentEditor.Payload (Db.Present (Input.JsonValue (Encode.list identity [])))

                        titleRestored =
                            Editor.fields edited
                                |> List.any (\field -> field.id == DocumentEditor.Title && field.original == Db.Present (Input.StringValue row.title) && field.current == field.original)
                    in
                    ( { model | editing = Just ( row.id, edited ) }
                    , editorReport
                        (Encode.object
                            [ ( "opened", Encode.bool (bad == Nothing && Editor.formError unavailable /= Nothing && same == Just Document.emptyPatch && sameKeys == Just Document.emptyPatch && sameGroups == Just Document.emptyPatch && titleRestored && arrayChange /= Just Document.emptyPatch && arrayChange /= Nothing) )
                            , ( "unavailable", Encode.bool (bad == Nothing && Editor.formError unavailable /= Nothing) )
                            , ( "same", Encode.bool (same == Just Document.emptyPatch) )
                            , ( "sameKeys", Encode.bool (sameKeys == Just Document.emptyPatch) )
                            , ( "sameGroups", Encode.bool (sameGroups == Just Document.emptyPatch) )
                            , ( "restored", Encode.bool titleRestored )
                            ]
                        )
                    )

                Nothing ->
                    ( model, report "opened" False )

        Action "submit-edit" ->
            case model.editing of
                Just ( id, form ) ->
                    let
                        ( result, checked ) =
                            Editor.validate form
                    in
                    ( { model | editing = Just ( id, checked ) }
                    , result
                        |> Maybe.map (\patch -> pyreStoreOut (Db.Edit.submit database "editor-update" [ Document.update id patch ]))
                        |> Maybe.withDefault Cmd.none
                    )

                Nothing ->
                    ( model, Cmd.none )

        Action key ->
            let
                ( queries, command ) =
                    Pyre.update (Pyre.QueryUpdate (Pyre.EditorDocuments database requestId { id = Db.Id.uuid key })) model.queries
            in
            ( { model | queries = queries }, effect command )

        QueryDelta wire ->
            let
                ( queries, command ) =
                    Pyre.update (Pyre.decodeIncomingDelta wire) model.queries

                next =
                    { model | queries = queries }
            in
            ( next
            , Cmd.batch
                [ effect command
                , syncedRow next |> Maybe.map (\row -> editorReport (Encode.object [ ( "queryTitle", Encode.string row.title ) ])) |> Maybe.withDefault Cmd.none
                ]
            )

        MutationResult wire ->
            ( model, editorReport wire )


syncedRow model =
    Pyre.getResult database requestId model.queries.editorDocuments |> Maybe.andThen (.document >> List.head)


effect command =
    case command of
        Pyre.Send value ->
            pyreStoreOut value

        _ ->
            Cmd.none


report check passed =
    editorReport (Encode.object [ ( check, Encode.bool passed ) ])


json source =
    Decode.decodeString Decode.value source |> Result.withDefault Encode.null


groups source =
    Dict.singleton "__proto__" [ Just (Db.Present_ { reason = "test", details = Just (json source) }) ]
