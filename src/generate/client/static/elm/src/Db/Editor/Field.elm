module Db.Editor.Field exposing (Error(..), View)

import Db


{-| Choices list the schema variant names for a union field. The namespace's
value type carries the actual union and its payload. If a historical value
cannot be written back, `historical` holds its read-only JSON and `original`
is `Db.Absent`. Restoring the baseline leaves that field out of the patch.
-}
type alias View fieldId kind value =
    { id : fieldId
    , kind : kind
    , required : Bool
    , nullable : Bool
    , choices : List String
    , historical : Maybe Db.Json
    , original : Db.Nullable value
    , current : Db.Nullable value
    , error : Maybe Error
    }


type Error
    = Required
    | NotNullable
    | WrongKind
