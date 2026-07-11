module Db.Read exposing
    ( Query, query
    , Decoder, succeed, field
    , bool, string, int, float, dateTime, nullable
    , custom
    , id, nested
    , decodeValue, andDecode, andDecodeIndex
    )

{-|

@docs Query, query

@docs Decoder, succeed, field

@docs bool, string, int, float, dateTime, nullable

@docs custom

@docs id, nested

@docs decodeValue, andDecode, andDecodeIndex

-}

import Json.Decode as Json
import Set exposing (Set)
import Time


type Decoder a
    = Decoder (Int -> Json.Value -> Json.Decoder a)


type Query a
    = Query
        { identity : List (Json.Decoder Id)
        , decoder : Decoder a
        }


andDecodeIndex : Int -> Query item -> Json.Decoder (List item -> result) -> Json.Decoder result
andDecodeIndex index (Query queryDetails) toResult =
    let
        (Decoder toDecoder) =
            queryDetails.decoder
    in
    toResult
        |> Json.andThen
            (\fn ->
                Json.map (\result -> fn (List.reverse result)) <|
                    Json.index index
                        (Json.value
                            |> Json.andThen
                                (\json ->
                                    uniqueListDecoder 0
                                        queryDetails.identity
                                        (Json.succeed True)
                                        (toDecoder 0 json)
                                )
                        )
            )


andDecode : String -> Query item -> Json.Decoder (List item -> result) -> Json.Decoder result
andDecode fieldname (Query queryDetails) toResult =
    let
        (Decoder toDecoder) =
            queryDetails.decoder
    in
    toResult
        |> Json.andThen
            (\fn ->
                Json.map (\result -> fn (List.reverse result)) <|
                    Json.field fieldname
                        (Json.value
                            |> Json.andThen
                                (\json ->
                                    uniqueListDecoder 0
                                        queryDetails.identity
                                        (Json.succeed True)
                                        (toDecoder 0 json)
                                )
                        )
            )


decodeValue : Query selected -> Json.Value -> Result Json.Error (List selected)
decodeValue (Query queryDetails) json =
    runDecoderWith 0 queryDetails.identity queryDetails.decoder (Json.succeed True) json


query : a -> List (IdField Id) -> Query a
query decoder identity =
    Query
        { identity = List.map .decoder identity
        , decoder = succeed decoder
        }


succeed : a -> Decoder a
succeed a =
    Decoder (\_ _ -> Json.succeed a)


nullable : Decoder a -> Decoder (Maybe a)
nullable (Decoder toInner) =
    Decoder (\index json -> Json.nullable (toInner index json))


custom : Json.Decoder a -> Decoder a
custom decoder =
    Decoder (\_ _ -> decoder)


int : Decoder Int
int =
    Decoder (\_ _ -> Json.int)


dateTime : Decoder Time.Posix
dateTime =
    Decoder (\_ _ -> Json.map (\seconds -> Time.millisToPosix (seconds * 1000)) Json.int)


string : Decoder String
string =
    Decoder (\_ _ -> Json.string)


bool : Decoder Bool
bool =
    Decoder
        (\_ _ ->
            Json.oneOf
                [ Json.map (\i -> i /= 0) Json.int
                , Json.bool
                ]
        )


float : Decoder Float
float =
    Decoder (\_ _ -> Json.float)


field : String -> Decoder a -> Query (a -> b) -> Query b
field fieldName_ (Decoder toFieldDecoder) (Query q) =
    let
        (Decoder toBuilder) =
            q.decoder
    in
    Query
        { identity = q.identity
        , decoder =
            Decoder
                (\index json ->
                    toBuilder index json
                        |> Json.andThen
                            (\func ->
                                Json.field fieldName_ (toFieldDecoder index json)
                                    |> Json.map func
                            )
                )
        }


type alias Id =
    Int


type alias IdField val =
    { name : String
    , decoder : Json.Decoder val
    }


id : String -> IdField Id
id name =
    { name = name
    , decoder = Json.field name Json.int
    }


toJsonDecoder : Int -> Json.Value -> Decoder a -> Json.Decoder a
toJsonDecoder index json (Decoder toInner) =
    toInner index json


nested : IdField Id -> IdField Id -> Query innerSelected -> Query (List innerSelected -> selected) -> Query selected
nested topLevelIdField innerId (Query innerQ) (Query topQ) =
    Query
        { identity = topQ.identity
        , decoder =
            Decoder
                (\topLevelIndex fullJson ->
                    Json.oneOf
                        [ topLevelIdField.decoder
                            |> Json.andThen
                                (\parentId ->
                                    let
                                        rowCheck =
                                            Json.map
                                                (\foundParentId ->
                                                    foundParentId == parentId
                                                )
                                                topLevelIdField.decoder
                                    in
                                    case runDecoderWith topLevelIndex innerQ.identity innerQ.decoder rowCheck fullJson of
                                        Ok captured ->
                                            Json.map
                                                (\fn ->
                                                    fn captured
                                                )
                                                (toJsonDecoder topLevelIndex fullJson topQ.decoder)

                                        Err err ->
                                            Json.fail "Failed decoding nested columns"
                                )
                        , Json.map
                            (\fn ->
                                fn []
                            )
                            (toJsonDecoder topLevelIndex fullJson topQ.decoder)
                        ]
                )
        }


runDecoderWith : Int -> List (Json.Decoder Id) -> Decoder selected -> Json.Decoder Bool -> Json.Value -> Result Json.Error (List selected)
runDecoderWith startingIndex uniqueBy (Decoder toDecoder) rowCheckDecoder json =
    Json.decodeValue
        (uniqueListDecoder startingIndex uniqueBy rowCheckDecoder (toDecoder startingIndex json))
        json
        |> Result.map List.reverse


uniqueListDecoder : Int -> List (Json.Decoder Id) -> Json.Decoder Bool -> Json.Decoder selected -> Json.Decoder (List selected)
uniqueListDecoder startingIndex uniqueBy rowCheck decoder =
    uniqueListDecoderHelper startingIndex Set.empty uniqueBy rowCheck [] decoder


uniqueListDecoderHelper : Int -> Set String -> List (Json.Decoder Id) -> Json.Decoder Bool -> List selected -> Json.Decoder selected -> Json.Decoder (List selected)
uniqueListDecoderHelper index found uniqueBy rowCheck foundList rowDecoder =
    Json.oneOf
        [ Json.index index rowCheck
            |> Json.andThen
                (\should_include ->
                    if should_include then
                        Json.index index (decodeCompoundId uniqueBy)
                            |> Json.andThen
                                (\compoundId ->
                                    Json.lazy
                                        (\_ ->
                                            if Set.member compoundId found && compoundId /= "" then
                                                uniqueListDecoderHelper (index + 1) found uniqueBy rowCheck foundList rowDecoder

                                            else
                                                Json.index index rowDecoder
                                                    |> Json.andThen
                                                        (\row ->
                                                            uniqueListDecoderHelper (index + 1)
                                                                (Set.insert compoundId found)
                                                                uniqueBy
                                                                rowCheck
                                                                (row :: foundList)
                                                                rowDecoder
                                                        )
                                        )
                                )

                    else
                        Json.fail "Failed row check"
                )
        , -- If indices still exist, keep going
          Json.index index (Json.succeed ())
            |> Json.andThen
                (\_ ->
                    Json.lazy
                        (\_ ->
                            uniqueListDecoderHelper (index + 1) found uniqueBy rowCheck foundList rowDecoder
                        )
                )
        , Json.lazy
            (\_ ->
                Json.succeed foundList
            )
        ]


decodeCompoundId : List (Json.Decoder Id) -> Json.Decoder String
decodeCompoundId uniqueBy =
    decodeCompoundIdHelper uniqueBy ""


decodeCompoundIdHelper : List (Json.Decoder Id) -> String -> Json.Decoder String
decodeCompoundIdHelper idDecoder str =
    case idDecoder of
        [] ->
            Json.succeed str

        decoder :: rest ->
            decoder
                |> Json.andThen
                    (\foundId ->
                        Json.lazy
                            (\_ ->
                                decodeCompoundIdHelper rest (str ++ "_" ++ idToString foundId)
                            )
                    )


idToString : Id -> String
idToString =
    String.fromInt
