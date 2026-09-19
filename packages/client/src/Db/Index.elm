module Db.Index exposing
    ( Index
    , IndexKey
    , RowId
    , buildIndicesFromSchema
    , empty
    , insert
    , lookup
    , rebuildFromTable
    , remove
    , update
    )

{-| Index module for efficient foreign key lookups.

An Index maps foreign key values to lists of row IDs, enabling O(1) lookups
instead of O(N) table scans for relationship resolution.


# Performance Improvement

Before indices: O(N) table scan for each OneToMany relationship
After indices: O(1) lookup + O(k) row retrieval where k = number of related rows

Example: Query 100 users with their posts (each user has ~50 posts)

    Before: 100 parent lookups × 1000 posts scanned = 100,000 iterations
    After: 100 lookups × 1 dict access = 100 operations


# Index Building Strategy

Indices are built for OneToMany relationships and any schema-declared indices.
ManyToOne and OneToOne relationships use primary key lookups which are already O(1).

Indices are:

  - Built from scratch when initial data loads from IndexedDB
  - Updated incrementally as deltas arrive from live sync

-}

import Data.Delta exposing (Delta, TableGroup)
import Data.Identity exposing (Key)
import Data.Schema exposing (LinkType(..), SchemaMetadata)
import Data.Value exposing (Value)
import Dict exposing (Dict)


{-| Opaque index type.

Internally, this is a Dict from IndexKey (foreign key value) to a list of RowIds.

-}
type Index
    = Index (Dict String (List Key))


{-| The foreign key value (e.g., "1", "2" for user\_id values).
-}
type alias IndexKey =
    String


{-| Row identifier (corresponds to SQLite rowid).
-}
type alias RowId =
    Key


{-| Create an empty index.
-}
empty : Index
empty =
    Index Dict.empty


{-| Insert a single row ID under a foreign key value.

If the IndexKey is already present, the RowId is added to the list.

-}
insert : IndexKey -> RowId -> Index -> Index
insert key rowId (Index dict) =
    Index <|
        Dict.update key
            (\maybeList ->
                case maybeList of
                    Just list ->
                        if List.member rowId list then
                            Just list

                        else
                            Just (rowId :: list)

                    Nothing ->
                        Just [ rowId ]
            )
            dict


{-| Remove a row ID from under a foreign key value.

If the list becomes empty, the key is removed from the index.

-}
remove : IndexKey -> RowId -> Index -> Index
remove key rowId (Index dict) =
    Index <|
        Dict.update key
            (\maybeList ->
                case maybeList of
                    Just list ->
                        let
                            filtered =
                                List.filter (\id -> id /= rowId) list
                        in
                        if List.isEmpty filtered then
                            Nothing

                        else
                            Just filtered

                    Nothing ->
                        Nothing
            )
            dict


{-| Update an index when a foreign key value changes.

This handles the case where a row's foreign key changes from oldKey to newKey.
If oldKey is Nothing, only insert. If newKey is Nothing, only remove.

The "before" state comes from the client's in-memory database state, which is
compared against the incoming delta's "after" state in `calculateIndexUpdates`.
This allows proper index updates when foreign keys change without requiring the
server to send both before/after data over the network.

-}
update : { oldKey : Maybe IndexKey, newKey : Maybe IndexKey, rowId : RowId } -> Index -> Index
update { oldKey, newKey, rowId } index =
    let
        afterRemove =
            case oldKey of
                Just key ->
                    remove key rowId index

                Nothing ->
                    index
    in
    case newKey of
        Just key ->
            insert key rowId afterRemove

        Nothing ->
            afterRemove


{-| Look up all row IDs associated with a foreign key value.

Returns an empty list if the key is not found.

-}
lookup : IndexKey -> Index -> List RowId
lookup key (Index dict) =
    Dict.get key dict
        |> Maybe.withDefault []


{-| Rebuild an index from scratch for a specific column in a table.

This scans all rows in the table and builds an index on the specified column.

-}
rebuildFromTable : Dict Key (Dict String Value) -> String -> Index
rebuildFromTable tableData columnName =
    Dict.foldl
        (\rowId row acc ->
            case Dict.get columnName row of
                Just value ->
                    case valueToIndexKey value of
                        Just key ->
                            insert key rowId acc

                        Nothing ->
                            -- Null foreign key, skip
                            acc

                Nothing ->
                    -- Column doesn't exist in this row, skip
                    acc
        )
        empty
        tableData


{-| Convert a Value to an IndexKey (String).

Returns Nothing for Null values (we don't index null foreign keys).

-}
valueToIndexKey : Value -> Maybe String
valueToIndexKey value =
    case value of
        Data.Value.IntValue i ->
            Just ("i:" ++ String.fromInt i)

        Data.Value.StringValue s ->
            Just ("s:" ++ s)

        Data.Value.NullValue ->
            Nothing

        _ ->
            -- Other types shouldn't be used as foreign keys
            Nothing


{-| Build all necessary indices from schema metadata.

This creates indices for OneToMany relationships (the problematic ones) and
for schema-declared indices.

Returns a Dict keyed by (tableName, columnName).

-}
buildIndicesFromSchema : SchemaMetadata -> Dict String (Dict Key (Dict String Value)) -> Dict ( String, String ) Index
buildIndicesFromSchema schema tables =
    Dict.foldl
        (\tableName tableMeta acc ->
            let
                accWithLinks =
                    Dict.foldl
                        (\linkName linkInfo innerAcc ->
                            case linkInfo.type_ of
                                OneToMany ->
                                    -- Build index on the target table's foreign key column
                                    let
                                        targetTable =
                                            Dict.get linkInfo.to.table tables |> Maybe.withDefault Dict.empty

                                        index =
                                            rebuildFromTable targetTable linkInfo.to.column
                                    in
                                    Dict.insert ( linkInfo.to.table, linkInfo.to.column ) index innerAcc

                                _ ->
                                    -- ManyToOne and OneToOne use primary key lookups (already O(1))
                                    innerAcc
                        )
                        acc
                        tableMeta.links

                accWithExplicit =
                    List.foldl
                        (\indexInfo innerAcc ->
                            let
                                tableRows =
                                    Dict.get tableName tables |> Maybe.withDefault Dict.empty

                                index =
                                    rebuildFromTable tableRows indexInfo.field
                            in
                            Dict.insert ( tableName, indexInfo.field ) index innerAcc
                        )
                        accWithLinks
                        tableMeta.indices
            in
            accWithExplicit
        )
        Dict.empty
        schema.tables
