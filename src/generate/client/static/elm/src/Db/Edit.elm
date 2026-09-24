module Db.Edit exposing (Edit, Receipt, receive, result, submit)

import Db.Database
import Db.Internal.Edit as Internal
import Json.Decode as Decode
import Json.Encode as Encode


type alias Edit namespace =
    Internal.Edit namespace


{-| A completion scoped to the database instance and request that submitted it.
-}
type Receipt namespace
    = Receipt (Result String (List Decode.Value))


receive : Db.Database.DatabaseId namespace -> String -> Decode.Value -> Result String (Receipt namespace)
receive databaseId requestId value =
    let
        decoder =
            Decode.map2 Tuple.pair
                (Decode.field "databaseId" Decode.string)
                (Decode.field "requestId" Decode.string)
                |> Decode.andThen
                    (\( actualDatabase, actualRequest ) ->
                        if actualDatabase /= Db.Database.toString databaseId || actualRequest /= requestId then
                            Decode.fail "Receipt belongs to another database or request"

                        else
                            Decode.field "result"
                                (Decode.field "ok" Decode.bool
                                    |> Decode.andThen
                                        (\ok ->
                                            if ok then
                                                Decode.map (Receipt << Ok) (Decode.field "value" (Decode.list Decode.value))

                                            else
                                                Decode.map (Receipt << Err) (Decode.field "error" Decode.string)
                                        )
                                )
                    )
    in
    Decode.decodeValue decoder value |> Result.mapError Decode.errorToString


{-| Generated record accessors supply the compiled query identity and decoder.
-}
result : String -> Decode.Decoder value -> Int -> Receipt namespace -> Result String value
result queryId decoder index (Receipt completion) =
    completion
        |> Result.andThen
            (\values ->
                case List.head (List.drop index values) of
                    Nothing ->
                        Err "Missing operation result"

                    Just value ->
                        Decode.decodeValue
                            (Decode.map2 Tuple.pair
                                (Decode.field "index" Decode.int)
                                (Decode.field "queryId" Decode.string)
                                |> Decode.andThen
                                    (\( actualIndex, actualQuery ) ->
                                        if index < 0 || actualIndex /= index || actualQuery /= queryId then
                                            Decode.fail "Operation result identity mismatch"

                                        else
                                            Decode.field "result" decoder
                                    )
                            )
                            value
                            |> Result.mapError Decode.errorToString
            )


{-| Submit an ordered atomic batch through the ordinary Pyre bridge port.
The runtime allocates create identities before installing optimistic intent.
-}
submit : Db.Database.DatabaseId namespace -> String -> List (Edit namespace) -> Encode.Value
submit databaseId requestId edits =
    Encode.object
        [ ( "type", Encode.string "submit" )
        , ( "databaseId", Db.Database.encode databaseId )
        , ( "requestId", Encode.string requestId )
        , ( "operations", Encode.list Internal.encode edits )
        ]
