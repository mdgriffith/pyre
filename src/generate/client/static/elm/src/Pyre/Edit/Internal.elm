module Pyre.Edit.Internal exposing (Batch, Edit, and, decodeInt, decodeUuid, decoder, operation, operations, single, succeed)

import Db.Id
import Json.Decode as D
import Json.Encode as E


type Edit namespace result
    = Edit E.Value (D.Decoder result)


type Batch namespace result
    = Batch (List E.Value) (Int -> D.Decoder result)


decodeInt : D.Decoder (Db.Id.Integer guard)
decodeInt =
    D.int
        |> D.andThen
            (\value ->
                if abs (toFloat value) <= 9007199254740991 then
                    D.succeed (Db.Id.int value)

                else
                    D.fail "Unsafe integer identity"
            )


decodeUuid : D.Decoder (Db.Id.Uuid guard)
decodeUuid =
    D.string
        |> D.andThen
            (\value ->
                let
                    parts =
                        String.split "-" value

                    hex =
                        String.all (\char -> String.contains (String.fromChar char) "0123456789abcdefABCDEF") (String.concat parts)
                in
                if List.map String.length parts == [ 8, 4, 4, 4, 12 ] && hex then
                    D.succeed (Db.Id.uuid value)

                else
                    D.fail "Invalid UUID identity"
            )


operation : String -> String -> String -> E.Value -> D.Decoder result -> Edit namespace result
operation namespace manifest id input decode =
    Edit (E.object [ ( "namespace", E.string namespace ), ( "manifest", E.string manifest ), ( "operation", E.string id ), ( "input", input ) ])
        (D.map2 Tuple.pair (D.field "operation" D.string) (D.field "value" decode)
            |> D.andThen
                (\( actual, value ) ->
                    if actual == id then
                        D.succeed value

                    else
                        D.fail "Operation mismatch"
                )
        )


succeed : a -> Batch n a
succeed value =
    Batch [] (\_ -> D.succeed value)


and : Edit n a -> Batch n (a -> b) -> Batch n b
and (Edit wire decode) (Batch previous partial) =
    Batch (previous ++ [ wire ])
        (\offset ->
            D.map2 (\fn value -> fn value)
                (partial offset)
                (D.index (offset + List.length previous)
                    (D.field "index" D.int
                        |> D.andThen
                            (\index ->
                                if index == offset + List.length previous then
                                    decode

                                else
                                    D.fail "Index mismatch"
                            )
                    )
                )
        )


single : Edit n a -> Batch n a
single edit =
    and edit (succeed identity)


operations : Batch n a -> List E.Value
operations (Batch values _) =
    values


decoder : Batch n a -> D.Decoder a
decoder (Batch values decode) =
    D.list D.value
        |> D.andThen
            (\results ->
                if List.length results == List.length values then
                    decode 0

                else
                    D.fail "Result count mismatch"
            )
