module Db.Query exposing
    ( FieldQuery
    , FilterOperator(..)
    , FilterValue(..)
    , Query
    , Selection(..)
    , SortClause
    , SortDirection(..)
    , WhereClause
    , decodeQuery
    , operatorFromString
    , operatorToString
    )

import Data.Value exposing (Value)
import Dict exposing (Dict)
import Json.Decode as Decode


type alias Query =
    Dict String FieldQuery


type alias FieldQuery =
    { selections : Dict String Selection
    , where_ : Maybe WhereClause
    , sort : Maybe (List SortClause)
    , limit : Maybe Int
    }


type Selection
    = SelectField (Maybe String)
    | SelectNested (Maybe String) FieldQuery


type alias WhereClause =
    Dict String FilterValue


type FilterOperator
    = OpEq
    | OpNe
    | OpGt
    | OpGte
    | OpLt
    | OpLte
    | OpIn
    | OpNotIn
    | OpLike
    | OpNotLike


type FilterValue
    = FilterValueSimple Value
    | FilterValueNull
    | FilterValueOperators (Dict String FilterValue)
    | FilterValueAnd (List WhereClause)
    | FilterValueOr (List WhereClause)


type alias SortClause =
    { field : String
    , direction : SortDirection
    }


type SortDirection
    = Asc
    | Desc


decodeQuery : Decode.Decoder Query
decodeQuery =
    Decode.dict Decode.value
        |> Decode.andThen
            (\fields ->
                case Dict.get "$error" fields of
                    Just error ->
                        case Decode.decodeValue Decode.string error of
                            Ok message ->
                                Decode.fail message

                            Err _ ->
                                Decode.fail "Invalid local query $error marker"

                    Nothing ->
                        if List.any (containsSessionReference QueryShape) (Dict.values fields) then
                            Decode.fail "Local queries cannot reference Session; use explicit inputs or execute on the server."

                        else
                            Decode.dict decodeFieldQuery
            )


type SessionReferenceContext
    = QueryShape
    | Predicate
    | Operand


-- Inspect raw shapes before permissive decoding, without treating JSON operands as predicates.
containsSessionReference : SessionReferenceContext -> Decode.Value -> Bool
containsSessionReference context value =
    case Decode.decodeValue (Decode.keyValuePairs Decode.value) value of
        Ok pairs ->
            let
                isSessionMarker =
                    case pairs of
                        [ ( "$session", name ) ] ->
                            Result.toMaybe (Decode.decodeValue Decode.string name) /= Nothing

                        _ ->
                            False
            in
            isSessionMarker
                || List.any
                    (\( key, child ) ->
                        case context of
                            QueryShape ->
                                containsSessionReference
                                    (if key == "@where" then
                                        Predicate

                                     else if String.startsWith "@" key then
                                        Operand

                                     else
                                        QueryShape
                                    )
                                    child

                            Predicate ->
                                String.startsWith "Session." key
                                    || containsSessionReference
                                        (if key == "$and" || key == "$or" then
                                            Predicate

                                         else
                                            Operand
                                        )
                                        child

                            Operand ->
                                containsSessionReference Operand child
                    )
                    pairs

        Err _ ->
            case Decode.decodeValue (Decode.list Decode.value) value of
                Ok items ->
                    List.any (containsSessionReference context) items

                Err _ ->
                    False


decodeFieldQuery : Decode.Decoder FieldQuery
decodeFieldQuery =
    Decode.keyValuePairs Decode.value
        |> Decode.andThen buildFieldQueryFromPairs


buildFieldQueryFromPairs : List ( String, Decode.Value ) -> Decode.Decoder FieldQuery
buildFieldQueryFromPairs pairs =
    buildFieldQueryFromPairsHelp Dict.empty Nothing Nothing Nothing pairs


buildFieldQueryFromPairsHelp :
    Dict String Selection
    -> Maybe WhereClause
    -> Maybe (List SortClause)
    -> Maybe Int
    -> List ( String, Decode.Value )
    -> Decode.Decoder FieldQuery
buildFieldQueryFromPairsHelp selections where_ sort limit pairs =
    case pairs of
        [] ->
            Decode.succeed
                { selections = selections
                , where_ = where_
                , sort = sort
                , limit = limit
                }

        ( key, value ) :: rest ->
            case key of
                "@where" ->
                    case Decode.decodeValue decodeWhereClause value of
                        Ok whereClause ->
                            buildFieldQueryFromPairsHelp selections (Just whereClause) sort limit rest

                        Err _ ->
                            Decode.fail "Invalid @where clause"

                "@sort" ->
                    case Decode.decodeValue decodeSortValue value of
                        Ok sortClauses ->
                            buildFieldQueryFromPairsHelp selections where_ (Just sortClauses) limit rest

                        Err _ ->
                            Decode.fail "Invalid @sort clause"

                "@limit" ->
                    case Decode.decodeValue Decode.int value of
                        Ok limitValue ->
                            buildFieldQueryFromPairsHelp selections where_ sort (Just limitValue) rest

                        Err _ ->
                            Decode.fail "Invalid @limit value"

                "@source" ->
                    buildFieldQueryFromPairsHelp selections where_ sort limit rest

                "@select" ->
                    buildFieldQueryFromPairsHelp selections where_ sort limit rest

                _ ->
                    -- Regular field selection (bool or nested)
                    case Decode.decodeValue decodeSelection value of
                        Ok selection ->
                            buildFieldQueryFromPairsHelp (Dict.insert key selection selections) where_ sort limit rest

                        Err _ ->
                            -- Skip invalid selections
                            buildFieldQueryFromPairsHelp selections where_ sort limit rest


decodeSelection : Decode.Decoder Selection
decodeSelection =
    Decode.oneOf
        [ Decode.bool
            |> Decode.andThen
                (\b ->
                    if b then
                        Decode.succeed (SelectField Nothing)

                    else
                        Decode.fail "false is not a valid selection"
                )
        , Decode.keyValuePairs Decode.value
            |> Decode.andThen decodeObjectSelection
        ]


decodeObjectSelection : List ( String, Decode.Value ) -> Decode.Decoder Selection
decodeObjectSelection pairs =
    let
        source =
            pairs
                |> List.filterMap
                    (\( key, value ) ->
                        if key == "@source" then
                            Decode.decodeValue Decode.string value |> Result.toMaybe

                        else
                            Nothing
                    )
                |> List.head

        isFieldSelection =
            pairs
                |> List.any
                    (\( key, value ) ->
                        key == "@select" && Decode.decodeValue Decode.bool value == Ok True
                    )
    in
    if isFieldSelection then
        Decode.succeed (SelectField source)

    else
        buildFieldQueryFromPairs pairs
            |> Decode.map (SelectNested source)


decodeSortValue : Decode.Decoder (List SortClause)
decodeSortValue =
    Decode.oneOf
        [ Decode.list decodeSortClause
        , decodeSortClause |> Decode.map (\s -> [ s ])
        ]


decodeWhereClause : Decode.Decoder WhereClause
decodeWhereClause =
    Decode.dict decodeFilterValue


decodeFilterValue : Decode.Decoder FilterValue
decodeFilterValue =
    Decode.oneOf
        [ Decode.null FilterValueNull
        , Decode.dict Decode.value
            |> Decode.andThen
                (\dict ->
                    if Dict.member "$and" dict then
                        Decode.field "$and" (Decode.list decodeWhereClause)
                            |> Decode.map FilterValueAnd

                    else if Dict.member "$or" dict then
                        Decode.field "$or" (Decode.list decodeWhereClause)
                            |> Decode.map FilterValueOr

                    else
                        decodeOperatorDict dict
                )
        , Data.Value.decodeValue |> Decode.map FilterValueSimple
        ]


decodeOperatorDict : Dict String Decode.Value -> Decode.Decoder FilterValue
decodeOperatorDict dict =
    let
        decodeEntry opString value acc =
            case operatorFromString opString of
                Just _ ->
                    case Decode.decodeValue decodeFilterValue value of
                        Ok filterValue ->
                            Decode.succeed (Dict.insert opString filterValue acc)

                        Err _ ->
                            Decode.fail "Invalid operator value"

                Nothing ->
                    Decode.fail "Unknown operator"
    in
    Dict.foldl
        (\key value decoderAcc ->
            decoderAcc
                |> Decode.andThen (\acc -> decodeEntry key value acc)
        )
        (Decode.succeed Dict.empty)
        dict
        |> Decode.map FilterValueOperators


operatorFromString : String -> Maybe FilterOperator
operatorFromString operator =
    case operator of
        "$eq" ->
            Just OpEq

        "$ne" ->
            Just OpNe

        "$gt" ->
            Just OpGt

        "$gte" ->
            Just OpGte

        "$lt" ->
            Just OpLt

        "$lte" ->
            Just OpLte

        "$in" ->
            Just OpIn

        "$nin" ->
            Just OpNotIn

        "$like" ->
            Just OpLike

        "$nlike" ->
            Just OpNotLike

        _ ->
            Nothing


operatorToString : FilterOperator -> String
operatorToString operator =
    case operator of
        OpEq ->
            "$eq"

        OpNe ->
            "$ne"

        OpGt ->
            "$gt"

        OpGte ->
            "$gte"

        OpLt ->
            "$lt"

        OpLte ->
            "$lte"

        OpIn ->
            "$in"

        OpNotIn ->
            "$nin"

        OpLike ->
            "$like"

        OpNotLike ->
            "$nlike"


decodeSortClause : Decode.Decoder SortClause
decodeSortClause =
    Decode.map2 SortClause
        (Decode.field "field" Decode.string)
        (Decode.field "direction" decodeSortDirection)


decodeSortDirection : Decode.Decoder SortDirection
decodeSortDirection =
    Decode.string
        |> Decode.andThen
            (\str ->
                case String.toLower str of
                    "desc" ->
                        Decode.succeed Desc

                    _ ->
                        Decode.succeed Asc
            )
