module Db.Editor exposing (Form, Kind(..), UnionInfo, Variant, VariantField, errors, fields, formError, set, validate)

import Db
import Db.Editor.Field as Field
import Db.Internal.Editor as Internal


{-| The shape of an editor payload. A generated namespace supplies its own
`UnionId` and lookup; the other constructors are shared across schemas.
-}
type Kind unionId
    = StringKind
    | IntKind
    | FloatKind
    | BoolKind
    | DateTimeKind
    | JsonKind
    | ListKind (Kind unionId)
    | DictKind (Kind unionId)
    | MaybeKind (Kind unionId)
    | IntIdKind String
    | UuidIdKind String
    | UnionKind unionId


type alias UnionInfo unionId =
    { name : String, variants : List (Variant unionId) }


type alias Variant unionId =
    { name : String, fields : List (VariantField unionId) }


type alias VariantField unionId =
    { name : String, kind : Kind unionId }


type alias Form fieldId kind value result =
    Internal.Form fieldId kind value result


fields : Form fieldId kind value result -> List (Field.View fieldId kind value)
fields =
    Internal.fields


{-| Use the ID supplied by the field view. `Db.Absent` resets a field to its
original value (or leaves it unset on a create form).
-}
set : fieldId -> Db.Nullable value -> Form fieldId kind value result -> Form fieldId kind value result
set =
    Internal.set


{-| Keep the returned form to display validation feedback.
-}
validate : Form fieldId kind value result -> ( Maybe result, Form fieldId kind value result )
validate =
    Internal.validate


errors : Form fieldId kind value result -> List ( fieldId, Field.Error )
errors =
    Internal.errors


formError : Form fieldId kind value result -> Maybe String
formError =
    Internal.formError
