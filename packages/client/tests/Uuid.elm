module Uuid exposing (id, set, value)

import Data.Value
import Set exposing (Set)


id : Int -> String
id n =
    "00000000-0000-7000-8000-" ++ String.padLeft 12 '0' (String.fromInt n)


value : Int -> Data.Value.Value
value n =
    Data.Value.StringValue (id n)


set : List Int -> Set String
set =
    List.map id >> Set.fromList
