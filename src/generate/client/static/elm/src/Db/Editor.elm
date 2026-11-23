module Db.Editor exposing (Form, errors, fields, formError, set, validate)

import Db
import Db.Editor.Field as Field
import Db.Internal.Editor as Internal


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
