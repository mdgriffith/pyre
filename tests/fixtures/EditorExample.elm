port module EditorExample exposing (main)

import Db
import Db.Database
import Db.Edit
import Db.Edit.Document.Draft as Document
import Db.Edit.Document.Editor as DocumentEditor
import Db.Editor as Editor
import Db.Editor.Field as Field
import Db.Editor.Input.Default as Input
import Db.Id
import Dict
import Json.Decode as Decode
import Json.Encode as Encode
import Platform
import Pyre


port pyreStoreOut : Encode.Value -> Cmd msg
port pyre_receiveQueryDelta : (Decode.Value -> msg) -> Sub msg
port pyre_receiveMutationResult : (Decode.Value -> msg) -> Sub msg
port editorAction : (String -> msg) -> Sub msg
port editorReport : Encode.Value -> Cmd msg


type Msg
    = Action String
    | QueryDelta Decode.Value
    | MutationResult Decode.Value


type alias Model =
    { queries : Pyre.Model
    , draft : Maybe Document.Draft
    , form : Maybe (Editor.Form Input.Input Document.Patch)
    , id : Maybe Db.Id.Document
    }


database =
    Db.Database.fromString "first"


requestId =
    "editor-doc"


main : Program () Model Msg
main =
    Platform.worker
        { init = \() -> ( { queries = Pyre.init, draft = Nothing, form = Nothing, id = Nothing }, Cmd.none )
        , update = update
        , subscriptions = \_ -> Sub.batch [ editorAction Action, pyre_receiveQueryDelta QueryDelta, pyre_receiveMutationResult MutationResult ]
        }


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        Action "check-create" ->
            let
                ( invalid, invalidForm ) =
                    Editor.validate DocumentEditor.createForm

                ( bad, badForm ) =
                    invalidForm
                        |> Editor.set "title" (Field.Value (Input.Document_Owner "wrong field"))
                        |> Editor.set "tags" Field.Null
                        |> Editor.validate

                repaired =
                    badForm |> Editor.set "title" (Field.Value (Input.Document_Title "Draft title"))

                ( valid, _ ) =
                    repaired
                        |> Editor.set "owner" (Field.Value (Input.Document_Owner "Owner"))
                        |> Editor.set "tags" (Field.Value (Input.Document_Tags [ "draft" ]))
                        |> Editor.set "status" (Field.Value (Input.Document_Status (Db.Closed { reason = "Done" })))
                        |> Editor.set "groups" (Field.Value (Input.Document_Groups (Dict.singleton "draft" [ Just Db.Open, Nothing ])))
                        |> Editor.validate

                ( invalidKey, unknown ) =
                    DocumentEditor.createForm |> Editor.set "notAField" Field.Null |> Editor.validate

                hasChoices =
                    Editor.fields DocumentEditor.createForm
                        |> List.any (\view -> view.metadata.key == "status" && view.metadata.shape == Field.Choices "Status" [ ( "Open", [] ), ( "Closed", [ ( "reason", Field.Scalar "String" ) ] ) ])
            in
            ( { model | draft = valid }
            , report "create" (invalid == Nothing && Editor.errors invalidForm == [ ( "owner", Field.Required ), ( "tags", Field.Required ), ( "title", Field.Required ) ]
                && bad == Nothing && List.member ( "title", Field.WrongInput ) (Editor.errors badForm)
                && List.member ( "tags", Field.NotNullable ) (Editor.errors badForm)
                && not (List.any (Tuple.first >> (==) "title") (Editor.errors repaired))
                && invalidKey == Nothing && Editor.formError unknown /= Nothing && hasChoices
                && Maybe.map .title valid == Just "Draft title"
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
                        edited =
                            DocumentEditor.editForm row
                                |> Editor.set "title" (Field.Value (Input.Document_Title "Local edit"))
                                |> Editor.set "tags" (Field.Value (Input.Document_Tags [ "whole", "replacement" ]))
                                |> Editor.set "caption" Field.Null
                                |> Editor.set "payload" (Field.Value (Input.Document_Payload (Encode.list identity [])))
                                |> Editor.set "title" (Field.Value (Input.Document_Title row.title))
                    in
                    ( { model | form = Just edited, id = Just row.id }, report "opened" True )

                Nothing ->
                    ( model, report "opened" False )

        Action "submit-edit" ->
            case ( model.id, model.form ) of
                ( Just id, Just form ) ->
                    let
                        ( result, validated ) =
                            Editor.validate form
                    in
                    ( { model | form = Just validated }
                    , result
                        |> Maybe.map (\patch -> pyreStoreOut (Db.Edit.submit database "editor-update" [ Document.update id patch ]))
                        |> Maybe.withDefault Cmd.none
                    )

                _ ->
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
        Pyre.Send value -> pyreStoreOut value
        _ -> Cmd.none


report check passed =
    editorReport (Encode.object [ ( check, Encode.bool passed ) ])
