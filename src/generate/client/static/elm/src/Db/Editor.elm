module Db.Editor exposing (Form, errors, fields, formError, set, validate)

{-| Typed, flat form state. Keep it in the application's model independently
of the synced row and submit the resulting draft or patch through `Db.Edit`.
-}

import Db.Editor.Field as Field
import Db.Internal.Editor as Internal


type alias Form input result =
    Internal.Form input result


fields : Form input result -> List (Field.View input)
fields =
    Internal.fields


{-| Change a top-level field by its schema key. `Unset` reverts an edit field
to its original value and leaves a create field unset.
-}
set : String -> Field.Value input -> Form input result -> Form input result
set =
    Internal.set


errors : Form input result -> List ( String, Field.Error )
errors =
    Internal.errors


formError : Form input result -> Maybe String
formError =
    Internal.formError


validate : Form input result -> ( Maybe result, Form input result )
validate =
    Internal.validate
