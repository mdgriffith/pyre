module Db.Internal.Edit exposing (Edit(..), encode)

import Json.Encode as Encode


type Edit namespace
    = Edit
        { queryId : String
        , input : Encode.Value
        , optimistic : Encode.Value
        , createId : Maybe String
        }


encode : Edit namespace -> Encode.Value
encode (Edit edit) =
    Encode.object
        [ ( "queryId", Encode.string edit.queryId )
        , ( "input", edit.input )
        , ( "optimistic", edit.optimistic )
        , ( "createId", Maybe.map Encode.string edit.createId |> Maybe.withDefault Encode.null )
        ]
