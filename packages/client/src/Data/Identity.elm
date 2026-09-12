module Data.Identity exposing (Key, fromRow, fromValue, int, toValue, uuid)

import Data.Schema exposing (PrimaryKeyKind(..), SchemaMetadata)
import Data.Value exposing (Value(..))
import Dict exposing (Dict)


{-| Comparable, type-separated keys. Integers retain numeric ordering.
Table and database scope belong to the containing dictionaries, not the wire ID.
-}
type alias Key =
    ( Int, Int, String )


int : Int -> Key
int value =
    ( 0, value, "" )


uuid : String -> Key
uuid value =
    ( 1, 0, value )


toValue : Key -> Value
toValue ( tag, number, text ) =
    if tag == 0 then
        IntValue number

    else
        StringValue text


fromValue : PrimaryKeyKind -> Value -> Result String Key
fromValue kind value =
    case ( kind, value ) of
        ( IntKey, IntValue number ) ->
            if number >= -9007199254740991 && number <= 9007199254740991 then
                Ok (int number)

            else
                Err "Integer primary key exceeds the safe wire range"

        ( UuidKey, StringValue text ) ->
            if
                List.map String.length (String.split "-" text)
                    == [ 8, 4, 4, 4, 12 ]
                    && String.all (\c -> c == '-' || Char.isDigit c || List.member c (String.toList "abcdefABCDEF")) text
            then
                Ok (uuid text)

            else
                Err "Invalid UUID primary key"

        _ ->
            Err "Primary key has the wrong type"


fromRow : SchemaMetadata -> String -> Dict String Value -> Result String Key
fromRow schema table row =
    case Dict.get table schema.tables of
        Nothing ->
            Err ("Missing table identity metadata: " ++ table)

        Just metadata ->
            if String.isEmpty metadata.primaryKey.name then
                Err ("Empty primary key metadata: " ++ table)

            else
                case Dict.get metadata.primaryKey.name row of
                    Nothing ->
                        Err ("Missing primary key: " ++ table ++ "." ++ metadata.primaryKey.name)

                    Just value ->
                        fromValue metadata.primaryKey.kind value
