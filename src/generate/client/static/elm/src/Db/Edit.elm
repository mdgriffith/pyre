module Db.Edit exposing (Edit, submit)

import Db.Database
import Db.Edit.Internal as Internal
import Json.Encode as Encode


type alias Edit namespace =
    Internal.Edit namespace


{-| Submit an ordered atomic batch through the ordinary Pyre bridge port.
The runtime allocates create identities before installing optimistic intent.
-}
submit : Db.Database.DatabaseId namespace -> String -> List (Edit namespace) -> Encode.Value
submit databaseId requestId edits =
    Encode.object
        [ ( "type", Encode.string "submit" )
        , ( "databaseId", Db.Database.encode databaseId )
        , ( "requestId", Encode.string requestId )
        , ( "operations", Encode.list Internal.encode edits )
        ]
