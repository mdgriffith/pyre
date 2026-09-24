module Data.RowId exposing (fromValue)

import Char
import Data.Value exposing (Value(..))


{-| Synced row keys are UUIDs. Named imports may use any UUID version;
UUIDv7 validation belongs to the generated-create execution boundary.
-}
fromValue : Value -> Maybe String
fromValue value =
    case value of
        StringValue id ->
            let
                parts =
                    String.split "-" id

                hex c =
                    Char.isDigit c || List.member (Char.toLower c) [ 'a', 'b', 'c', 'd', 'e', 'f' ]
            in
            if List.map String.length parts == [ 8, 4, 4, 4, 12 ] && List.all (String.all hex) parts then
                Just id

            else
                Nothing

        _ ->
            Nothing
