port module Main exposing (main)

import Browser
import Html exposing (Html, div, h1, li, p, text, ul)
import Html.Attributes exposing (style)
import Json.Decode as Decode
import Json.Encode as Encode
import Pyre
import Query.ListUsersAndPosts



-- MODEL


type alias Model =
    { pyre : Pyre.Model
    }


init : () -> ( Model, Cmd Msg )
init _ =
    ( { pyre = Pyre.init "playground" }
    , Cmd.none
    )



-- MSG


type Msg
    = PyreMsg Pyre.Msg
    | RegisterQuery



-- UPDATE


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        PyreMsg pyreMsg ->
            let
                ( newPyre, pyreEffect ) =
                    Pyre.update pyreMsg model.pyre
            in
            ( { model | pyre = newPyre }
            , effectToCmd pyreEffect
            )

        RegisterQuery ->
            -- Example: register a query with id "main-query"
            let
                ( newPyre, pyreEffect ) =
                    Pyre.update
                        (Pyre.QueryUpdate (Pyre.ListUsersAndPosts "main-query" {}))
                        model.pyre
            in
            ( { model | pyre = newPyre }
            , effectToCmd pyreEffect
            )


effectToCmd : Pyre.Effect -> Cmd Msg
effectToCmd effect =
    case effect of
        Pyre.NoEffect ->
            Cmd.none

        Pyre.Send payload ->
            pyre_sendQueryClientMessage payload

        Pyre.QueryUpdated _ ->
            Cmd.none

        Pyre.LogError payload ->
            pyre_logQueryDeltaError payload



-- VIEW


view : Model -> Html Msg
view model =
    div [ style "padding" "20px", style "font-family" "sans-serif" ]
        [ h1 [] [ text "Pyre QueryClient Demo" ]
        , viewQueryResult model
        ]


viewQueryResult : Model -> Html Msg
viewQueryResult model =
    case Pyre.getResult "main-query" model.pyre.listUsersAndPosts of
        Just result ->
            div []
                [ h2 [] [ text "Users" ]
                , ul [] (List.map viewUser result.user)
                , h2 [] [ text "Posts" ]
                , ul [] (List.map viewPost result.post)
                ]

        Nothing ->
            p [] [ text "No query registered yet" ]


h2 : List (Html.Attribute msg) -> List (Html msg) -> Html msg
h2 =
    Html.h2


viewUser : Query.ListUsersAndPosts.User -> Html Msg
viewUser user =
    li []
        [ text ("User #" ++ String.fromInt user.id ++ ": " ++ Maybe.withDefault "(no name)" user.name)
        ]


viewPost : Query.ListUsersAndPosts.Post -> Html Msg
viewPost post =
    li []
        [ text ("Post #" ++ String.fromInt post.id ++ ": " ++ post.title)
        ]



-- SUBSCRIPTIONS


subscriptions : Model -> Sub Msg
subscriptions _ =
    pyre_receiveQueryDelta (PyreMsg << Pyre.decodeIncomingDelta)



-- PORTS


port pyre_sendQueryClientMessage : Encode.Value -> Cmd msg


port pyre_receiveQueryDelta : (Decode.Value -> msg) -> Sub msg


port pyre_logQueryDeltaError : Encode.Value -> Cmd msg



-- MAIN


main : Program () Model Msg
main =
    Browser.element
        { init = init
        , update = update
        , view = view
        , subscriptions = subscriptions
        }
