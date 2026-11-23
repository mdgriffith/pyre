module Db.Internal.Edit exposing (CreateIdentity, Edit(..), createIdentity, encode)

import Json.Encode as Encode


type Edit namespace
    = Edit
        { queryId : String
        , input : Encode.Value
        , optimistic : Encode.Value
        , createId : Maybe CreateIdentity
        }


type CreateIdentity
    = Automatic String
    | Explicit String Encode.Value
    | Duplicate


createIdentity : String -> List Encode.Value -> Maybe CreateIdentity
createIdentity field ids =
    Just
        (case ids of
            [] ->
                Automatic field

            [ id ] ->
                Explicit field id

            _ ->
                Duplicate
        )


encodeIdentity : CreateIdentity -> Encode.Value
encodeIdentity identity =
    case identity of
        Automatic field ->
            Encode.object [ ( "mode", Encode.string "auto" ), ( "field", Encode.string field ) ]

        Explicit field id ->
            Encode.object [ ( "mode", Encode.string "explicit" ), ( "field", Encode.string field ), ( "id", id ) ]

        Duplicate ->
            Encode.object [ ( "mode", Encode.string "duplicate" ) ]


encode : Edit namespace -> Encode.Value
encode (Edit edit) =
    Encode.object
        [ ( "queryId", Encode.string edit.queryId )
        , ( "input", edit.input )
        , ( "optimistic", edit.optimistic )
        , ( "createId", Maybe.map encodeIdentity edit.createId |> Maybe.withDefault Encode.null )
        ]
