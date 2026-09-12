module Data.Delta exposing (Delta, TableGroup, decodeDelta, decodeTableGroup, encodeDelta, encodeTableGroup, validate)

import Data.Identity exposing (Key)
import Data.Schema exposing (SchemaMetadata)
import Data.Value exposing (Value)
import Dict exposing (Dict)
import Json.Decode as Decode
import Json.Encode as Encode
import Set exposing (Set)


{-| Delta contains table groups, where each group has multiple rows for a single table.
This grouped format is more efficient than sending individual rows.
-}
type alias Delta =
    { tableGroups : List TableGroup
    }


{-| A group of rows for a single table.
Rows are stored as arrays (not objects) to minimize bandwidth.
Headers provide the mapping from array indices to column names.
-}
type alias TableGroup =
    { tableName : String
    , headers : List String
    , rows : List (List Value)
    }


decodeDelta : Decode.Decoder Delta
decodeDelta =
    Decode.map Delta
        (Decode.list decodeTableGroup)


decodeTableGroup : Decode.Decoder TableGroup
decodeTableGroup =
    Decode.map3 TableGroup
        (Decode.field "table_name" Decode.string)
        (Decode.field "headers" (Decode.list Decode.string))
        (Decode.field "rows" (Decode.list (Decode.list Data.Value.decodeValue)))
        |> Decode.andThen
            (\group ->
                if validShape group then
                    Decode.succeed group

                else
                    Decode.fail "Invalid delta row width or duplicate headers"
            )


validShape : TableGroup -> Bool
validShape group =
    Set.size (Set.fromList group.headers)
        == List.length group.headers
        && List.all (\row -> List.length row == List.length group.headers) group.rows


{-| Validate every group before storage, tracking or persistence. Repeated groups
are merged, but duplicate identities within one message reject the entire message.
-}
validate : SchemaMetadata -> Delta -> Result String (Dict String (Set Key))
validate schema delta =
    List.foldl
        (\group acc ->
            if not (validShape group) then
                Err "Invalid delta row width or duplicate headers"

            else if not (Dict.member group.tableName schema.tables) then
                Err ("Missing table identity metadata: " ++ group.tableName)

            else
                List.foldl
                    (\row result ->
                        Result.map2 Tuple.pair
                            result
                            (Data.Identity.fromRow schema group.tableName (Dict.fromList (List.map2 Tuple.pair group.headers row)))
                            |> Result.andThen
                                (\( tables, key ) ->
                                    let
                                        keys =
                                            Dict.get group.tableName tables |> Maybe.withDefault Set.empty
                                    in
                                    if Set.member key keys then
                                        Err ("Duplicate primary key in " ++ group.tableName)

                                    else
                                        Ok (Dict.insert group.tableName (Set.insert key keys) tables)
                                )
                    )
                    acc
                    group.rows
        )
        (Ok Dict.empty)
        delta.tableGroups


encodeDelta : Delta -> Encode.Value
encodeDelta delta =
    Encode.list encodeTableGroup delta.tableGroups


encodeTableGroup : TableGroup -> Encode.Value
encodeTableGroup group =
    Encode.object
        [ ( "table_name", Encode.string group.tableName )
        , ( "headers", Encode.list Encode.string group.headers )
        , ( "rows", Encode.list (Encode.list Data.Value.encodeValue) group.rows )
        ]
