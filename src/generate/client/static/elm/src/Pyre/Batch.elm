module Pyre.Batch exposing (Batch, and, succeed)

import Pyre.Edit exposing (Edit)
import Pyre.Edit.Internal as Internal


type alias Batch namespace result =
    Internal.Batch namespace result


succeed : a -> Batch n a
succeed =
    Internal.succeed


and : Edit n a -> Batch n (a -> b) -> Batch n b
and =
    Internal.and
