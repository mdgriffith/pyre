module Pyre.Edit exposing (Created, Deleted, Edit, Updated)

import Pyre.Edit.Internal


type alias Edit namespace result =
    Pyre.Edit.Internal.Edit namespace result


type alias Created id =
    { id : id }


type alias Updated id =
    { id : id }


type alias Deleted id =
    { id : id }
