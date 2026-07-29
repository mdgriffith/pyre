module Db exposing (Db, Msg(..), QueryExecutionResult, executeQuery, executeQueryWithTracking, extractAffectedTables, fromInitialData, init, rowMatchesWhere, update)

import Basics exposing (Order(..))
import Data.Delta exposing (Delta, TableGroup)
import Data.IndexedDb
import Data.Schema exposing (SchemaMetadata)
import Data.Value exposing (Value)
import Db.Index
import Db.Query
import Dict exposing (Dict)
import Set exposing (Set)



-- Database state in memory


type alias Db =
    { tables : Dict String TableData
    , indices : Dict ( String, String ) Db.Index.Index
    }


type alias TableData =
    Dict Int (Dict String Value)



-- Database Messages


type Msg
    = FromIndexedDb SchemaMetadata Data.IndexedDb.Incoming
    | DeltaReceived Delta
    | LocalDeltaReceived Delta



-- Initialize an empty database


init : Db
init =
    { tables = Dict.empty
    , indices = Dict.empty
    }



-- Update database based on messages


update : Msg -> Db -> ( Db, Cmd Msg )
update msg db =
    case msg of
        FromIndexedDb schema incoming ->
            case incoming of
                Data.IndexedDb.InitialDataReceived initialData ->
                    ( fromInitialData schema initialData
                    , Cmd.none
                    )

                Data.IndexedDb.DatabaseEpochResetCompleted _ ->
                    ( db, Cmd.none )

                Data.IndexedDb.DatabaseEpochResetFailed _ _ ->
                    ( db, Cmd.none )

        DeltaReceived delta ->
            let
                updatedDb =
                    applyDelta delta db
            in
            ( updatedDb
            , Data.IndexedDb.writeDelta delta.tableGroups
            )

        LocalDeltaReceived delta ->
            ( applyDelta delta db
            , Cmd.none
            )



-- Convert initial data to database format


fromInitialData : SchemaMetadata -> Data.IndexedDb.InitialData -> Db
fromInitialData schema initialData =
    let
        tables =
            convertInitialDataToTableData initialData

        indices =
            Db.Index.buildIndicesFromSchema schema tables
    in
    { tables = tables
    , indices = indices
    }



-- Apply a delta to the database and update indices


applyDelta : Data.Delta.Delta -> Db -> Db
applyDelta delta db =
    let
        ( updatedTables, indexUpdates ) =
            applyDeltaToTableData db.indices db.tables delta

        updatedIndices =
            applyIndexUpdates indexUpdates db.indices
    in
    { tables = updatedTables
    , indices = updatedIndices
    }



-- Query execution result with tracking


type alias QueryExecutionResult =
    { results : Dict String (List (Dict String Value))
    , rowIds : Dict String (Set Int)
    }



-- Execute a query against the database


executeQuery : SchemaMetadata -> Db -> Db.Query.Query -> Dict String (List (Dict String Value))
executeQuery schema db query =
    Dict.map (\queryFieldName fieldQuery -> executeFieldQuery schema db.tables db.indices queryFieldName fieldQuery) query



-- Execute a query and track which row IDs are in the result set


executeQueryWithTracking : SchemaMetadata -> Db -> Db.Query.Query -> QueryExecutionResult
executeQueryWithTracking schema db query =
    let
        resultsWithIds =
            Dict.map
                (\queryFieldName fieldQuery ->
                    executeFieldQueryWithTracking schema db.tables db.indices queryFieldName fieldQuery
                )
                query

        results =
            Dict.map (\_ ( rows, _ ) -> rows) resultsWithIds

        rowIds =
            Dict.map (\_ ( _, ids ) -> ids) resultsWithIds
    in
    { results = results
    , rowIds = rowIds
    }



-- Extract affected table names from a delta


extractAffectedTables : Data.Delta.Delta -> List String
extractAffectedTables delta =
    List.foldl
        (\tableGroup acc ->
            if List.member tableGroup.tableName acc then
                acc

            else
                tableGroup.tableName :: acc
        )
        []
        delta.tableGroups



-- Helper: Convert initial data format to TableData format


convertInitialDataToTableData : Data.IndexedDb.InitialData -> Dict String TableData
convertInitialDataToTableData initialData =
    Dict.map
        (\tableName rows ->
            List.filterMap
                (\row ->
                    case getRowId row of
                        Just id ->
                            Just ( id, row )

                        Nothing ->
                            Nothing
                )
                rows
                |> Dict.fromList
        )
        initialData.tables



-- Helper: Get row ID from a row dictionary


getRowId : Dict String Value -> Maybe Int
getRowId row =
    case Dict.get "id" row of
        Just (Data.Value.IntValue i) ->
            Just i

        _ ->
            Nothing



-- Index update tracking


{-| Represents a change to an index that needs to be applied.
-}
type alias IndexUpdate =
    { indexKey : ( String, String )
    , oldKey : Maybe String
    , newKey : Maybe String
    , rowId : Int
    }


{-| Calculate index updates needed when a row changes.
Compares the old row (if it exists) with the new row to determine
which indices need to be updated.
-}
calculateIndexUpdates : Dict ( String, String ) Db.Index.Index -> String -> Int -> Maybe (Dict String Value) -> Dict String Value -> List IndexUpdate
calculateIndexUpdates indices tableName rowId existingRow newRow =
    Dict.foldl
        (\( idxTable, idxColumn ) _ acc ->
            if idxTable == tableName then
                let
                    oldKey =
                        existingRow
                            |> Maybe.andThen (Dict.get idxColumn)
                            |> Maybe.andThen valueToIndexKey

                    newKey =
                        Dict.get idxColumn newRow
                            |> Maybe.andThen valueToIndexKey
                in
                -- Only create update if keys actually changed
                if oldKey /= newKey then
                    { indexKey = ( idxTable, idxColumn )
                    , oldKey = oldKey
                    , newKey = newKey
                    , rowId = rowId
                    }
                        :: acc

                else
                    acc

            else
                acc
        )
        []
        indices


{-| Apply a list of index updates to the indices dictionary.
-}
applyIndexUpdates : List IndexUpdate -> Dict ( String, String ) Db.Index.Index -> Dict ( String, String ) Db.Index.Index
applyIndexUpdates updates indices =
    List.foldl
        (\indexUpdate accIndices ->
            case Dict.get indexUpdate.indexKey accIndices of
                Just index ->
                    let
                        updatedIndex =
                            Db.Index.update
                                { oldKey = indexUpdate.oldKey
                                , newKey = indexUpdate.newKey
                                , rowId = indexUpdate.rowId
                                }
                                index
                    in
                    Dict.insert indexUpdate.indexKey updatedIndex accIndices

                Nothing ->
                    accIndices
        )
        indices
        updates



-- Helper: Apply delta to TableData


applyDeltaToTableData : Dict ( String, String ) Db.Index.Index -> Dict String TableData -> Data.Delta.Delta -> ( Dict String TableData, List IndexUpdate )
applyDeltaToTableData indices data delta =
    List.foldl
        (\tableGroup ( accTables, accUpdates ) ->
            let
                tableName =
                    tableGroup.tableName

                currentTable =
                    Dict.get tableName accTables |> Maybe.withDefault Dict.empty

                ( updatedTable, indexUpdates ) =
                    applyTableGroupRows indices tableName currentTable tableGroup.headers tableGroup.rows
            in
            ( Dict.insert tableName updatedTable accTables
            , accUpdates ++ indexUpdates
            )
        )
        ( data, [] )
        delta.tableGroups


{-| Apply multiple rows from a table group to a table.
Converts row arrays to row objects using headers.
Returns the updated table and a list of index updates that need to be applied.
-}
applyTableGroupRows : Dict ( String, String ) Db.Index.Index -> String -> TableData -> List String -> List (List Value) -> ( TableData, List IndexUpdate )
applyTableGroupRows indices tableName table headers rows =
    List.foldl
        (\rowArray ( accTable, accUpdates ) ->
            let
                rowObj =
                    rowArrayToObject headers rowArray
            in
            case getRowId rowObj of
                Just rowId ->
                    let
                        existingRow =
                            Dict.get rowId accTable

                        -- Calculate index updates for this row
                        indexUpdates =
                            calculateIndexUpdates indices tableName rowId existingRow rowObj

                        -- Update table
                        updatedTable =
                            Dict.insert rowId rowObj accTable
                    in
                    ( updatedTable, accUpdates ++ indexUpdates )

                Nothing ->
                    -- Can't insert row without valid ID
                    ( accTable, accUpdates )
        )
        ( table, [] )
        rows


{-| Convert a row array to a row object using headers.
-}
rowArrayToObject : List String -> List Value -> Dict String Value
rowArrayToObject headers values =
    List.map2 Tuple.pair headers values
        |> Dict.fromList



-- Query execution helpers (moved from Query.elm)


executeFieldQuery : SchemaMetadata -> Dict String TableData -> Dict ( String, String ) Db.Index.Index -> String -> Db.Query.FieldQuery -> List (Dict String Value)
executeFieldQuery schema data indices queryFieldName fieldQuery =
    case Dict.get queryFieldName schema.queryFieldToTable of
        Just tableName ->
            case Dict.get tableName data of
                Just tableRows ->
                    applyWhereWithIndices indices tableName fieldQuery.where_ tableRows
                        |> applySort fieldQuery.sort
                        |> applyLimit fieldQuery.limit
                        |> projectFields schema tableName fieldQuery.selections data indices

                Nothing ->
                    []

        Nothing ->
            []


executeFieldQueryWithTracking : SchemaMetadata -> Dict String TableData -> Dict ( String, String ) Db.Index.Index -> String -> Db.Query.FieldQuery -> ( List (Dict String Value), Set Int )
executeFieldQueryWithTracking schema data indices queryFieldName fieldQuery =
    case Dict.get queryFieldName schema.queryFieldToTable of
        Just tableName ->
            case Dict.get tableName data of
                Just tableRows ->
                    let
                        -- Get all rows with their IDs
                        rowsWithIds =
                            Dict.toList tableRows

                        -- Apply WHERE filter (keep track of IDs)
                        filteredWithIds =
                            applyWhereWithIndicesWithIds indices tableName fieldQuery.where_ rowsWithIds

                        -- Apply sort (IDs stay paired)
                        sortedWithIds =
                            applySortWithIds fieldQuery.sort filteredWithIds

                        -- Apply limit
                        limitedWithIds =
                            applyLimitWithIds fieldQuery.limit sortedWithIds

                        -- Extract IDs before projection
                        rowIds =
                            limitedWithIds
                                |> List.map Tuple.first
                                |> Set.fromList

                        -- Project fields
                        rows =
                            limitedWithIds
                                |> List.map Tuple.second
                                |> projectFields schema tableName fieldQuery.selections data indices
                    in
                    ( rows, rowIds )

                Nothing ->
                    ( [], Set.empty )

        Nothing ->
            ( [], Set.empty )


evaluateWhereOnRow : Maybe Db.Query.WhereClause -> Dict String Value -> Bool
evaluateWhereOnRow whereClause row =
    case whereClause of
        Just where_ ->
            evaluateFilter row where_

        Nothing ->
            True


applyWhereWithIndices : Dict ( String, String ) Db.Index.Index -> String -> Maybe Db.Query.WhereClause -> TableData -> List (Dict String Value)
applyWhereWithIndices indices tableName whereClause tableRows =
    let
        filteredRowsWithIds =
            applyWhereWithIndicesWithIds indices tableName whereClause (Dict.toList tableRows)
    in
    List.map Tuple.second filteredRowsWithIds


applyWhereWithIndicesWithIds : Dict ( String, String ) Db.Index.Index -> String -> Maybe Db.Query.WhereClause -> List ( Int, Dict String Value ) -> List ( Int, Dict String Value )
applyWhereWithIndicesWithIds indices tableName whereClause rowsWithIds =
    case whereClause of
        Just where_ ->
            let
                maybeIndexedIds =
                    collectIndexedRowIds indices tableName where_
            in
            case maybeIndexedIds of
                Just indexedIds ->
                    rowsWithIds
                        |> List.filter (\( rowId, row ) -> Set.member rowId indexedIds && evaluateFilter row where_)

                Nothing ->
                    rowsWithIds
                        |> List.filter (\( _, row ) -> evaluateFilter row where_)

        Nothing ->
            rowsWithIds


collectIndexedRowIds : Dict ( String, String ) Db.Index.Index -> String -> Db.Query.WhereClause -> Maybe (Set Int)
collectIndexedRowIds indices tableName whereClause =
    let
        directFieldOperators =
            Dict.foldl
                (\field value acc ->
                    case acc of
                        Just _ ->
                            acc

                        Nothing ->
                            case value of
                                Db.Query.FilterValueOperators operators ->
                                    extractIndexedIdsFromOperators indices tableName field operators

                                Db.Query.FilterValueSimple simpleValue ->
                                    extractIndexedIdsFromOperators indices tableName field (Dict.fromList [ ( Db.Query.operatorToString Db.Query.OpEq, Db.Query.FilterValueSimple simpleValue ) ])

                                _ ->
                                    Nothing
                )
                Nothing
                whereClause

        nestedAnd =
            case Dict.get "$and" whereClause of
                Just (Db.Query.FilterValueAnd clauses) ->
                    collectIndexedFromAnd indices tableName clauses

                _ ->
                    Nothing
    in
    case directFieldOperators of
        Just _ ->
            directFieldOperators

        Nothing ->
            nestedAnd


collectIndexedFromAnd : Dict ( String, String ) Db.Index.Index -> String -> List Db.Query.WhereClause -> Maybe (Set Int)
collectIndexedFromAnd indices tableName clauses =
    case clauses of
        [] ->
            Nothing

        clause :: rest ->
            case collectIndexedRowIds indices tableName clause of
                Just ids ->
                    Just ids

                Nothing ->
                    collectIndexedFromAnd indices tableName rest


extractIndexedIdsFromOperators : Dict ( String, String ) Db.Index.Index -> String -> String -> Dict String Db.Query.FilterValue -> Maybe (Set Int)
extractIndexedIdsFromOperators indices tableName field operators =
    let
        indexKey =
            ( tableName, field )

        gatherIds opValue =
            case Dict.get indexKey indices of
                Just index ->
                    case valueToIndexKey opValue of
                        Just key ->
                            Db.Index.lookup key index |> Set.fromList |> Just

                        Nothing ->
                            Nothing

                Nothing ->
                    Nothing

        eqValue =
            Dict.get (Db.Query.operatorToString Db.Query.OpEq) operators
                |> Maybe.andThen
                    (\opValue ->
                        case opValue of
                            Db.Query.FilterValueSimple value ->
                                Just value

                            _ ->
                                Nothing
                    )

        inValues =
            Dict.get (Db.Query.operatorToString Db.Query.OpIn) operators
                |> Maybe.andThen
                    (\opValue ->
                        case opValue of
                            Db.Query.FilterValueSimple (Data.Value.ArrayValue values) ->
                                Just values

                            _ ->
                                Nothing
                    )
    in
    case eqValue of
        Just value ->
            gatherIds value

        Nothing ->
            case inValues of
                Just values ->
                    case Dict.get indexKey indices of
                        Just index ->
                            values
                                |> List.filterMap valueToIndexKey
                                |> List.concatMap (\key -> Db.Index.lookup key index)
                                |> Set.fromList
                                |> Just

                        Nothing ->
                            Nothing

                Nothing ->
                    Nothing


rowMatchesWhere : Maybe Db.Query.WhereClause -> Dict String Value -> Bool
rowMatchesWhere =
    evaluateWhereOnRow


applySortWithIds : Maybe (List Db.Query.SortClause) -> List ( Int, Dict String Value ) -> List ( Int, Dict String Value )
applySortWithIds sortClauses rowsWithIds =
    case sortClauses of
        Just clauses ->
            List.sortWith
                (\( _, a ) ( _, b ) ->
                    case
                        List.foldl
                            (\clause acc ->
                                case acc of
                                    Just _ ->
                                        acc

                                    Nothing ->
                                        let
                                            aVal =
                                                Dict.get clause.field a |> Maybe.withDefault Data.Value.NullValue

                                            bVal =
                                                Dict.get clause.field b |> Maybe.withDefault Data.Value.NullValue

                                            comparison =
                                                compareValues aVal bVal

                                            result =
                                                if clause.direction == Db.Query.Desc then
                                                    -1 * comparison

                                                else
                                                    comparison
                                        in
                                        if result < 0 then
                                            Just LT

                                        else if result > 0 then
                                            Just GT

                                        else
                                            Nothing
                            )
                            Nothing
                            clauses
                    of
                        Just order ->
                            order

                        Nothing ->
                            EQ
                )
                rowsWithIds

        Nothing ->
            rowsWithIds


applyLimitWithIds : Maybe Int -> List ( Int, Dict String Value ) -> List ( Int, Dict String Value )
applyLimitWithIds limit rowsWithIds =
    case limit of
        Just n ->
            List.take n rowsWithIds

        Nothing ->
            rowsWithIds


{-| Project (select) specific fields from rows based on the query selections.

If selections are empty, returns all fields from all rows.
Otherwise, returns only the selected fields.

Examples:

  - Selection { "id": SelectField, "name": SelectField }
    Row { "id": 1, "name": "Alice", "email": "alice@example.com" }
    → { "id": 1, "name": "Alice" }

  - Selection { "posts": SelectNested ... }
    Row { "id": 1, "name": "Alice" }
    → { "id": 1, "name": "Alice", "posts": [{ "id": 10, "title": "..." }, ...] }

-}
projectFields : SchemaMetadata -> String -> Dict String Db.Query.Selection -> Dict String TableData -> Dict ( String, String ) Db.Index.Index -> List (Dict String Value) -> List (Dict String Value)
projectFields schema tableName selections data indices rows =
    if Dict.isEmpty selections then
        rows

    else
        List.map (\row -> projectRow schema tableName row selections data indices) rows


projectRow : SchemaMetadata -> String -> Dict String Value -> Dict String Db.Query.Selection -> Dict String TableData -> Dict ( String, String ) Db.Index.Index -> Dict String Value
projectRow schema tableName row selections data indices =
    Dict.foldl
        (\fieldName selection acc ->
            case selection of
                Db.Query.SelectField maybeSourceField ->
                    let
                        sourceFieldName =
                            Maybe.withDefault fieldName maybeSourceField
                    in
                    case Dict.get sourceFieldName row of
                        Just value ->
                            Dict.insert fieldName value acc

                        Nothing ->
                            acc

                Db.Query.SelectNested maybeSourceField nestedFieldQuery ->
                    let
                        sourceFieldName =
                            Maybe.withDefault fieldName maybeSourceField
                    in
                    case getLinkType schema tableName sourceFieldName of
                        Just Data.Schema.OneToMany ->
                            let
                                relatedRows =
                                    resolveRelationship schema tableName sourceFieldName row data indices
                                        |> Maybe.withDefault []

                                nestedValue =
                                    projectNestedRows schema tableName sourceFieldName nestedFieldQuery relatedRows data indices
                            in
                            Dict.insert fieldName nestedValue acc

                        Just _ ->
                            case resolveRelationship schema tableName sourceFieldName row data indices of
                                Just relatedRows ->
                                    case projectNestedRows schema tableName sourceFieldName nestedFieldQuery relatedRows data indices of
                                        Data.Value.ArrayValue (first :: _) ->
                                            Dict.insert fieldName first acc

                                        Data.Value.ArrayValue [] ->
                                            Dict.insert fieldName Data.Value.NullValue acc

                                        value ->
                                            Dict.insert fieldName value acc

                                Nothing ->
                                    Dict.insert fieldName Data.Value.NullValue acc

                        Nothing ->
                            if isOneToManySelection schema tableName sourceFieldName then
                                Dict.insert fieldName (Data.Value.ArrayValue []) acc

                            else
                                Dict.insert fieldName Data.Value.NullValue acc
        )
        Dict.empty
        selections


projectNestedRows : SchemaMetadata -> String -> String -> Db.Query.FieldQuery -> List (Dict String Value) -> Dict String TableData -> Dict ( String, String ) Db.Index.Index -> Value
projectNestedRows schema tableName fieldName nestedFieldQuery relatedRows data indices =
    let
        relatedTableName =
            getRelatedTableName schema tableName fieldName

        projected =
            List.map (\r -> projectRow schema relatedTableName r nestedFieldQuery.selections data indices) relatedRows
    in
    Data.Value.ArrayValue (List.map Data.Value.ObjectValue projected)


getLinkType : SchemaMetadata -> String -> String -> Maybe Data.Schema.LinkType
getLinkType schema tableName fieldName =
    Dict.get tableName schema.tables
        |> Maybe.andThen (.links >> Dict.get fieldName)
        |> Maybe.map .type_


isOneToManySelection : SchemaMetadata -> String -> String -> Bool
isOneToManySelection schema tableName fieldName =
    getLinkType schema tableName fieldName == Just Data.Schema.OneToMany


resolveRelationship : SchemaMetadata -> String -> String -> Dict String Value -> Dict String TableData -> Dict ( String, String ) Db.Index.Index -> Maybe (List (Dict String Value))
resolveRelationship schema tableName fieldName row data indices =
    case Dict.get tableName schema.tables of
        Just tableMeta ->
            case Dict.get fieldName tableMeta.links of
                Just linkInfo ->
                    case linkInfo.type_ of
                        Data.Schema.OneToMany ->
                            case Dict.get "id" row of
                                Just idValue ->
                                    lookupRowsByForeignKeyIndexed indices data linkInfo.to.table linkInfo.to.column idValue
                                        |> Maybe.withDefault []
                                        |> Just

                                _ ->
                                    Just []

                        Data.Schema.ManyToOne ->
                            case Dict.get linkInfo.from row of
                                Just foreignKeyValue ->
                                    lookupRowByPrimaryKey data linkInfo.to.table linkInfo.to.column foreignKeyValue
                                        |> Maybe.map List.singleton

                                Nothing ->
                                    Nothing

                        Data.Schema.OneToOne ->
                            case Dict.get linkInfo.from row of
                                Just foreignKeyValue ->
                                    lookupRowByPrimaryKey data linkInfo.to.table linkInfo.to.column foreignKeyValue
                                        |> Maybe.map List.singleton

                                Nothing ->
                                    Nothing

                Nothing ->
                    Nothing

        Nothing ->
            Nothing


getRelatedTableName : SchemaMetadata -> String -> String -> String
getRelatedTableName schema tableName fieldName =
    case Dict.get tableName schema.tables of
        Just tableMeta ->
            case Dict.get fieldName tableMeta.links of
                Just linkInfo ->
                    linkInfo.to.table

                Nothing ->
                    ""

        Nothing ->
            ""


lookupRowsByForeignKey : Dict String TableData -> String -> String -> Value -> Maybe (List (Dict String Value))
lookupRowsByForeignKey data tableName foreignKeyColumn foreignKeyValue =
    case Dict.get tableName data of
        Just tableRows ->
            let
                matchingRows =
                    Dict.values tableRows
                        |> List.filter (\row -> Dict.get foreignKeyColumn row == Just foreignKeyValue)
            in
            Just matchingRows

        Nothing ->
            Nothing


{-| Lookup rows by foreign key using an index if available, otherwise fall back to linear scan.

This function provides the performance optimization for OneToMany relationships:

  - If an index exists for (table, column), uses O(1) lookup
  - Otherwise falls back to O(N) table scan

The query engine uses this automatically when resolving OneToMany relationships.

-}
lookupRowsByForeignKeyIndexed : Dict ( String, String ) Db.Index.Index -> Dict String TableData -> String -> String -> Value -> Maybe (List (Dict String Value))
lookupRowsByForeignKeyIndexed indices data tableName foreignKeyColumn foreignKeyValue =
    let
        indexKey =
            ( tableName, foreignKeyColumn )
    in
    case ( Dict.get indexKey indices, valueToIndexKey foreignKeyValue ) of
        ( Just index, Just fkValue ) ->
            -- Use index for O(1) lookup
            let
                rowIds =
                    Db.Index.lookup fkValue index

                rows =
                    case Dict.get tableName data of
                        Just tableRows ->
                            List.filterMap (\rowId -> Dict.get rowId tableRows) rowIds

                        Nothing ->
                            []
            in
            Just rows

        _ ->
            -- Fall back to linear scan
            lookupRowsByForeignKey data tableName foreignKeyColumn foreignKeyValue


valueToIndexKey : Value -> Maybe String
valueToIndexKey value =
    case value of
        Data.Value.IntValue i ->
            Just (String.fromInt i)

        Data.Value.StringValue s ->
            Just s

        Data.Value.NullValue ->
            Nothing

        _ ->
            Nothing


lookupRowByPrimaryKey : Dict String TableData -> String -> String -> Value -> Maybe (Dict String Value)
lookupRowByPrimaryKey data tableName primaryKeyColumn primaryKeyValue =
    case Dict.get tableName data of
        Just tableRows ->
            case valueToInt primaryKeyValue of
                Just id ->
                    Dict.get id tableRows

                Nothing ->
                    Nothing

        Nothing ->
            Nothing


valueToInt : Value -> Maybe Int
valueToInt value =
    case value of
        Data.Value.IntValue i ->
            Just i

        _ ->
            Nothing


applyWhere : Maybe Db.Query.WhereClause -> List (Dict String Value) -> List (Dict String Value)
applyWhere whereClause rows =
    case whereClause of
        Just where_ ->
            List.filter (\row -> evaluateFilter row where_) rows

        Nothing ->
            rows


evaluateFilter : Dict String Value -> Db.Query.WhereClause -> Bool
evaluateFilter row whereClause =
    case Dict.get "$and" whereClause of
        Just (Db.Query.FilterValueAnd clauses) ->
            List.all (\clause -> evaluateFilter row clause) clauses

        _ ->
            case Dict.get "$or" whereClause of
                Just (Db.Query.FilterValueOr clauses) ->
                    List.any (\clause -> evaluateFilter row clause) clauses

                _ ->
                    Dict.foldl
                        (\field condition acc ->
                            if not acc then
                                False

                            else
                                case Dict.get field row of
                                    Just fieldValue ->
                                        evaluateFilterValue fieldValue condition

                                    Nothing ->
                                        evaluateFilterValue Data.Value.NullValue condition
                        )
                        True
                        whereClause


evaluateFilterValue : Value -> Db.Query.FilterValue -> Bool
evaluateFilterValue fieldValue condition =
    case condition of
        Db.Query.FilterValueNull ->
            fieldValue == Data.Value.NullValue

        Db.Query.FilterValueSimple value ->
            fieldValue == value

        Db.Query.FilterValueOperators operators ->
            Dict.foldl
                (\opKey opValue acc ->
                    if not acc then
                        False

                    else
                        case Db.Query.operatorFromString opKey of
                            Just operator ->
                                evaluateOperator fieldValue operator opValue

                            Nothing ->
                                False
                )
                True
                operators

        Db.Query.FilterValueAnd _ ->
            False

        Db.Query.FilterValueOr _ ->
            False


evaluateOperator : Value -> Db.Query.FilterOperator -> Db.Query.FilterValue -> Bool
evaluateOperator fieldValue operator opValue =
    case ( operator, opValue ) of
        ( Db.Query.OpEq, Db.Query.FilterValueSimple value ) ->
            fieldValue == value

        ( Db.Query.OpNe, Db.Query.FilterValueSimple value ) ->
            fieldValue /= value

        ( Db.Query.OpGt, Db.Query.FilterValueSimple value ) ->
            compareValues fieldValue value > 0

        ( Db.Query.OpGte, Db.Query.FilterValueSimple value ) ->
            compareValues fieldValue value >= 0

        ( Db.Query.OpLt, Db.Query.FilterValueSimple value ) ->
            compareValues fieldValue value < 0

        ( Db.Query.OpLte, Db.Query.FilterValueSimple value ) ->
            compareValues fieldValue value <= 0

        ( Db.Query.OpIn, Db.Query.FilterValueSimple (Data.Value.ArrayValue values) ) ->
            List.member fieldValue values

        _ ->
            False


compareValues : Value -> Value -> Int
compareValues a b =
    case ( a, b ) of
        ( Data.Value.IntValue i1, Data.Value.IntValue i2 ) ->
            i1 - i2

        ( Data.Value.FloatValue f1, Data.Value.FloatValue f2 ) ->
            if f1 > f2 then
                1

            else if f1 < f2 then
                -1

            else
                0

        ( Data.Value.StringValue s1, Data.Value.StringValue s2 ) ->
            if s1 > s2 then
                1

            else if s1 < s2 then
                -1

            else
                0

        _ ->
            0


applySort : Maybe (List Db.Query.SortClause) -> List (Dict String Value) -> List (Dict String Value)
applySort sortClauses rows =
    case sortClauses of
        Just clauses ->
            List.sortWith
                (\a b ->
                    case
                        List.foldl
                            (\clause acc ->
                                case acc of
                                    Just _ ->
                                        acc

                                    Nothing ->
                                        let
                                            aVal =
                                                Dict.get clause.field a |> Maybe.withDefault Data.Value.NullValue

                                            bVal =
                                                Dict.get clause.field b |> Maybe.withDefault Data.Value.NullValue

                                            comparison =
                                                compareValues aVal bVal

                                            result =
                                                if clause.direction == Db.Query.Desc then
                                                    -1 * comparison

                                                else
                                                    comparison
                                        in
                                        if result < 0 then
                                            Just LT

                                        else if result > 0 then
                                            Just GT

                                        else
                                            Nothing
                            )
                            Nothing
                            clauses
                    of
                        Just order ->
                            order

                        Nothing ->
                            EQ
                )
                rows

        Nothing ->
            rows


applyLimit : Maybe Int -> List (Dict String Value) -> List (Dict String Value)
applyLimit limit rows =
    case limit of
        Just n ->
            List.take n rows

        Nothing ->
            rows
