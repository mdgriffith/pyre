port module Data.QueryManager exposing (Incoming(..), Model, Msg(..), OptimisticMutation, OptimisticSetField, OptimisticWhere, QueryClientIncoming(..), QueryDeltaOp(..), QuerySubscription, ReExecuteDecision(..), decodeIncoming, decodeQueryClientIncoming, doesChangeAffectWhereClause, extractChangedRowIds, extractWhereClauseFields, init, mutationResult, notifyTablesChanged, queryClientDelta, queryClientFull, receiveIncoming, receiveQueryClientIncoming, shouldReExecuteQuery, update)

import Data.Delta
import Data.Schema
import Data.Value exposing (Value)
import Db
import Db.Query
import Dict exposing (Dict)
import Json.Decode as Decode
import Json.Encode as Encode
import Set exposing (Set)



-- Model


type alias Model =
    { subscriptions : Dict String QuerySubscription
    }


type alias QuerySubscription =
    { queryId : String
    , query : Db.Query.Query
    , input : Encode.Value
    , callbackPort : String
    , resultRowIds : Dict String (Set Int)
    , revision : Int
    , lastResult : Maybe (Dict String (List (Dict String Value)))
    }



-- Messages


type Msg
    = IncomingReceived Incoming


type Incoming
    = SendMutation String String String (List ( String, String )) String Bool Encode.Value (Maybe OptimisticMutation) -- requestId, mutationId, baseUrl, headers, credentials, withCredentials, input, optimistic metadata


type alias OptimisticMutation =
    { queryField : String
    , where_ : OptimisticWhere
    , set : List OptimisticSetField
    }


type alias OptimisticWhere =
    { field : String
    , input : String
    }


type alias OptimisticSetField =
    { field : String
    , input : String
    }


{-| Incoming messages from QueryClient (TypeScript side)
-}
type QueryClientIncoming
    = QCRegister String Db.Query.Query Encode.Value -- queryId, querySource (as query shape), queryInput
    | QCUpdateInput String (Maybe Db.Query.Query) Encode.Value -- queryId, maybe querySource, queryInput
    | QCUnregister String -- queryId


type Message
    = QueryResult String Encode.Value -- callbackPort, result
    | QueryFull String Int Encode.Value -- queryId, revision, result
    | QueryDelta String Int (List QueryDeltaOp) -- queryId, revision, delta ops
    | MutationResult String String (Result String Encode.Value) -- requestId, mutationId, result


type QueryDeltaOp
    = SetRow String (Dict String Value)
    | RemoveRow String
    | InsertRow String Int (Dict String Value)
    | MoveRow String Int Int
    | RemoveRowByIndex String Int



-- Init


init : Model
init =
    { subscriptions = Dict.empty
    }



-- Update


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        IncomingReceived incoming ->
            handleIncoming incoming model


handleIncoming : Incoming -> Model -> ( Model, Cmd Msg )
handleIncoming incoming model =
    case incoming of
        SendMutation _ _ _ _ _ _ _ _ ->
            -- Mutations are handled by Main, not QueryManager
            ( model, Cmd.none )



-- Notify that tables have changed (with fine-grained reactivity)


notifyTablesChanged : Data.Schema.SchemaMetadata -> Db.Db -> Model -> Data.Delta.Delta -> ( Model, List (Cmd msg) )
notifyTablesChanged schema db model delta =
    Dict.foldl
        (\queryId subscription ( accModel, accCmds ) ->
            let
                -- Use fine-grained reactivity to decide if re-execution is needed
                decision =
                    shouldReExecuteQuery schema db subscription delta
            in
            case decision of
                ReExecuteFull ->
                    let
                        executionResult =
                            Db.executeQueryWithTracking schema db subscription.query

                        resultJson =
                            encodeQueryResult executionResult.results

                        deltaOutcome =
                            case subscription.lastResult of
                                Just previousResult ->
                                    buildDeltaOps previousResult executionResult.results

                                Nothing ->
                                    Err "Missing previous result"

                        updatedSubscriptionBase =
                            { subscription
                                | resultRowIds = executionResult.rowIds
                                , lastResult = Just executionResult.results
                            }
                    in
                    case deltaOutcome of
                        Ok ops ->
                            if List.isEmpty ops then
                                let
                                    subscriptionsAfterNoOp =
                                        Dict.insert queryId updatedSubscriptionBase accModel.subscriptions

                                    updatedModel =
                                        { accModel | subscriptions = subscriptionsAfterNoOp }
                                in
                                ( updatedModel, accCmds )

                            else
                                let
                                    nextRevision =
                                        subscription.revision + 1

                                    updatedSubscription =
                                        { updatedSubscriptionBase | revision = nextRevision }

                                    updatedSubscriptions =
                                        Dict.insert queryId updatedSubscription accModel.subscriptions

                                    updatedModel =
                                        { accModel | subscriptions = updatedSubscriptions }
                                in
                                ( updatedModel
                                , queryClientDelta subscription.queryId nextRevision ops :: accCmds
                                )

                        Err _ ->
                            let
                                nextRevision =
                                    subscription.revision + 1

                                updatedSubscription =
                                    { updatedSubscriptionBase | revision = nextRevision }

                                updatedSubscriptions =
                                    Dict.insert queryId updatedSubscription accModel.subscriptions

                                updatedModel =
                                    { accModel | subscriptions = updatedSubscriptions }
                            in
                            ( updatedModel
                            , queryClientFull subscription.queryId nextRevision resultJson :: accCmds
                            )

                NoReExecute ->
                    -- Delta doesn't affect this query, skip re-execution
                    ( accModel, accCmds )
        )
        ( model, [] )
        model.subscriptions


extractQueryTables : Data.Schema.SchemaMetadata -> Db.Query.Query -> List String
extractQueryTables schema query =
    Dict.foldl
        (\queryFieldName fieldQuery acc ->
            case Dict.get queryFieldName schema.queryFieldToTable of
                Just tableName ->
                    addTablesUnique acc (extractTablesFromFieldQuery schema tableName fieldQuery)

                Nothing ->
                    acc
        )
        []
        query


extractTablesFromFieldQuery : Data.Schema.SchemaMetadata -> String -> Db.Query.FieldQuery -> List String
extractTablesFromFieldQuery schema tableName fieldQuery =
    Dict.foldl
        (\fieldName selection acc ->
            case selection of
                Db.Query.SelectField _ ->
                    acc

                Db.Query.SelectNested maybeSourceField nestedFieldQuery ->
                    case relatedTableForSelection schema tableName (Maybe.withDefault fieldName maybeSourceField) of
                        Just relatedTable ->
                            extractTablesFromFieldQuery schema relatedTable nestedFieldQuery
                                |> (::) relatedTable
                                |> addTablesUnique acc

                        Nothing ->
                            acc
        )
        [ tableName ]
        fieldQuery.selections


relatedTableForSelection : Data.Schema.SchemaMetadata -> String -> String -> Maybe String
relatedTableForSelection schema tableName fieldName =
    Dict.get tableName schema.tables
        |> Maybe.andThen (\tableMeta -> Dict.get fieldName tableMeta.links)
        |> Maybe.map (\linkInfo -> linkInfo.to.table)


addTablesUnique : List String -> List String -> List String
addTablesUnique acc newTables =
    List.foldl
        (\tableName current ->
            if List.member tableName current then
                current

            else
                tableName :: current
        )
        acc
        newTables


encodeQueryResult : Dict String (List (Dict String Value)) -> Encode.Value
encodeQueryResult result =
    Encode.dict identity
        (\rows ->
            Encode.list (\row -> Encode.dict identity Data.Value.encodeValue row) rows
        )
        result



-- QueryDelta generation


type Id
    = IdInt Int
    | IdString String


buildDeltaOps : Dict String (List (Dict String Value)) -> Dict String (List (Dict String Value)) -> Result String (List QueryDeltaOp)
buildDeltaOps previousResult nextResult =
    let
        allFields =
            List.foldl
                (\field acc ->
                    if List.member field acc then
                        acc

                    else
                        field :: acc
                )
                (Dict.keys previousResult)
                (Dict.keys nextResult)
    in
    List.foldl
        (\field acc ->
            case acc of
                Err _ ->
                    acc

                Ok opsSoFar ->
                    case diffField field previousResult nextResult of
                        Ok fieldOps ->
                            Ok (opsSoFar ++ fieldOps)

                        Err err ->
                            Err err
        )
        (Ok [])
        allFields


diffField : String -> Dict String (List (Dict String Value)) -> Dict String (List (Dict String Value)) -> Result String (List QueryDeltaOp)
diffField fieldName previousResult nextResult =
    let
        oldRows =
            Dict.get fieldName previousResult |> Maybe.withDefault []

        newRows =
            Dict.get fieldName nextResult |> Maybe.withDefault []
    in
    case ( listRowIds oldRows, listRowIds newRows ) of
        ( Just oldIds, Just newIds ) ->
            let
                listOps =
                    buildListOps fieldName oldIds newIds newRows

                setOps =
                    buildSetOps fieldName oldIds newIds oldRows newRows
            in
            Ok (listOps ++ setOps)

        _ ->
            Err ("Missing id field for query result rows in " ++ fieldName)


buildListOps : String -> List Id -> List Id -> List (Dict String Value) -> List QueryDeltaOp
buildListOps fieldName oldIds newIds newRows =
    let
        oldKeys =
            List.map idKey oldIds

        newKeys =
            List.map idKey newIds

        newRowsByKey =
            Dict.fromList (List.map2 (\id row -> ( idKey id, row )) newIds newRows)

        ( opsAfterMoves, workingKeys ) =
            List.foldl
                (\( index, key ) ( ops, keys ) ->
                    if List.member key keys then
                        case listIndexOf key keys of
                            Just currentIndex ->
                                if currentIndex == index then
                                    ( ops, keys )

                                else
                                    let
                                        nextKeys =
                                            moveInList currentIndex index keys
                                    in
                                    ( ops ++ [ MoveRow (listPath fieldName) currentIndex index ], nextKeys )

                            Nothing ->
                                ( ops, keys )

                    else
                        case Dict.get key newRowsByKey of
                            Just row ->
                                let
                                    nextKeys =
                                        insertAt index key keys
                                in
                                ( ops ++ [ InsertRow (listPath fieldName) index row ], nextKeys )

                            Nothing ->
                                ( ops, keys )
                )
                ( [], oldKeys )
                (List.indexedMap Tuple.pair newKeys)

        removeOps =
            removeTrailingRows fieldName (List.length newKeys) workingKeys
    in
    opsAfterMoves ++ removeOps


buildSetOps : String -> List Id -> List Id -> List (Dict String Value) -> List (Dict String Value) -> List QueryDeltaOp
buildSetOps fieldName oldIds newIds oldRows newRows =
    let
        oldRowsByKey =
            Dict.fromList (List.map2 (\id row -> ( idKey id, row )) oldIds oldRows)

        newPairs =
            List.map2 Tuple.pair newIds newRows
    in
    List.indexedMap Tuple.pair newPairs
        |> List.filterMap
            (\( index, ( rowId, row ) ) ->
                let
                    key =
                        idKey rowId
                in
                case Dict.get key oldRowsByKey of
                    Just oldRow ->
                        if rowEquals oldRow row then
                            Nothing

                        else
                            Just (SetRow (rowPath fieldName index) row)

                    Nothing ->
                        Nothing
            )


rowEquals : Dict String Value -> Dict String Value -> Bool
rowEquals left right =
    if Dict.size left /= Dict.size right then
        False

    else
        Dict.foldl
            (\key value acc ->
                if not acc then
                    False

                else
                    case Dict.get key right of
                        Just otherValue ->
                            valueEquals value otherValue

                        Nothing ->
                            False
            )
            True
            left


valueEquals : Value -> Value -> Bool
valueEquals left right =
    case ( left, right ) of
        ( Data.Value.StringValue a, Data.Value.StringValue b ) ->
            a == b

        ( Data.Value.IntValue a, Data.Value.IntValue b ) ->
            a == b

        ( Data.Value.FloatValue a, Data.Value.FloatValue b ) ->
            a == b

        ( Data.Value.BoolValue a, Data.Value.BoolValue b ) ->
            a == b

        ( Data.Value.NullValue, Data.Value.NullValue ) ->
            True

        ( Data.Value.ArrayValue a, Data.Value.ArrayValue b ) ->
            listValueEquals a b

        ( Data.Value.ObjectValue a, Data.Value.ObjectValue b ) ->
            rowEquals a b

        _ ->
            False


listValueEquals : List Value -> List Value -> Bool
listValueEquals left right =
    if List.length left /= List.length right then
        False

    else
        List.all identity (List.map2 valueEquals left right)


listRowIds : List (Dict String Value) -> Maybe (List Id)
listRowIds rows =
    rows
        |> List.map extractRowId
        |> sequenceMaybe


extractRowId : Dict String Value -> Maybe Id
extractRowId row =
    case Dict.get "id" row of
        Just (Data.Value.IntValue id) ->
            Just (IdInt id)

        Just (Data.Value.StringValue id) ->
            Just (IdString id)

        _ ->
            Nothing


idKey : Id -> String
idKey id =
    case id of
        IdInt value ->
            "i:" ++ String.fromInt value

        IdString value ->
            "s:" ++ value


listPath : String -> String
listPath fieldName =
    "." ++ fieldName


rowPath : String -> Int -> String
rowPath fieldName index =
    "." ++ fieldName ++ "[" ++ String.fromInt index ++ "]"


listIndexOf : String -> List String -> Maybe Int
listIndexOf target items =
    let
        step item ( index, found ) =
            case found of
                Just _ ->
                    ( index + 1, found )

                Nothing ->
                    if item == target then
                        ( index + 1, Just index )

                    else
                        ( index + 1, Nothing )
    in
    items
        |> List.foldl step ( 0, Nothing )
        |> Tuple.second


insertAt : Int -> a -> List a -> List a
insertAt index value list =
    let
        before =
            List.take index list

        after =
            List.drop index list
    in
    before ++ (value :: after)


moveInList : Int -> Int -> List a -> List a
moveInList fromIndex toIndex list =
    if fromIndex == toIndex then
        list

    else
        let
            before =
                List.take fromIndex list

            rest =
                List.drop fromIndex list
        in
        case rest of
            item :: after ->
                let
                    without =
                        before ++ after

                    insertBefore =
                        List.take toIndex without

                    insertAfter =
                        List.drop toIndex without
                in
                insertBefore ++ (item :: insertAfter)

            _ ->
                list


removeTrailingRows : String -> Int -> List String -> List QueryDeltaOp
removeTrailingRows fieldName keepLength keys =
    let
        total =
            List.length keys

        removeIndices =
            List.range keepLength (total - 1)
    in
    removeIndices
        |> List.reverse
        |> List.map (\index -> RemoveRowByIndex (listPath fieldName) index)


sequenceMaybe : List (Maybe a) -> Maybe (List a)
sequenceMaybe values =
    List.foldr
        (\item acc ->
            case ( item, acc ) of
                ( Just value, Just rest ) ->
                    Just (value :: rest)

                _ ->
                    Nothing
        )
        (Just [])
        values



-- Fine-grained reactivity helpers


{-| Extract row IDs that changed from a delta, grouped by table name.
-}
extractChangedRowIds : Data.Delta.Delta -> Dict String (Set Int)
extractChangedRowIds delta =
    List.foldl
        (\tableGroup acc ->
            let
                changedIds =
                    List.filterMap
                        (\row ->
                            -- Row is a list of values, first one should be id
                            case row of
                                (Data.Value.IntValue id) :: _ ->
                                    Just id

                                _ ->
                                    Nothing
                        )
                        tableGroup.rows
                        |> Set.fromList
            in
            Dict.insert tableGroup.tableName changedIds acc
        )
        Dict.empty
        delta.tableGroups


type ReExecuteDecision
    = NoReExecute
    | ReExecuteFull


{-| Extract all field names referenced in a WHERE clause.

This includes fields in all nested conditions ($and, $or, operators, etc.)

-}
extractWhereClauseFields : Db.Query.WhereClause -> Set String
extractWhereClauseFields whereClause =
    Dict.foldl
        (\fieldName filterValue acc ->
            case filterValue of
                Db.Query.FilterValueAnd clauses ->
                    -- Recursively extract from nested AND clauses
                    List.foldl
                        (\clause innerAcc ->
                            Set.union innerAcc (extractWhereClauseFields clause)
                        )
                        acc
                        clauses

                Db.Query.FilterValueOr clauses ->
                    -- Recursively extract from nested OR clauses
                    List.foldl
                        (\clause innerAcc ->
                            Set.union innerAcc (extractWhereClauseFields clause)
                        )
                        acc
                        clauses

                Db.Query.FilterValueOperators _ ->
                    -- Field is being filtered
                    Set.insert fieldName acc

                Db.Query.FilterValueSimple _ ->
                    -- Field is being filtered
                    Set.insert fieldName acc

                Db.Query.FilterValueNull ->
                    -- Field is being filtered
                    Set.insert fieldName acc
        )
        Set.empty
        whereClause


{-| Check if a change to a row affects a WHERE clause.

Compares the old and new row values for fields referenced in the WHERE clause.
Returns True if any filtered field changed, False otherwise.

-}
doesChangeAffectWhereClause : Db.Query.WhereClause -> Dict String Value -> Dict String Value -> Bool
doesChangeAffectWhereClause whereClause oldRow newRow =
    let
        filteredFields =
            extractWhereClauseFields whereClause
    in
    -- Check if any filtered field changed
    Set.foldl
        (\fieldName hasChanged ->
            if hasChanged then
                -- Already found a change, short circuit
                True

            else
                -- Check if this field changed
                Dict.get fieldName oldRow /= Dict.get fieldName newRow
        )
        False
        filteredFields


{-| Determine if a query should be re-executed based on delta changes.

Implementation (Phase 3): Row-level filtering with WHERE clause analysis

  - Check if delta tables overlap with query tables
  - Check if changed row IDs overlap with result row IDs
  - For updates: analyze if filtered fields changed
  - For potential inserts: always re-execute (might match WHERE)

-}
shouldReExecuteQuery : Data.Schema.SchemaMetadata -> Db.Db -> QuerySubscription -> Data.Delta.Delta -> ReExecuteDecision
shouldReExecuteQuery schema db subscription delta =
    let
        -- Get tables used by this query
        queryTables =
            extractQueryTables schema subscription.query

        -- Get changed row IDs grouped by table
        changedRowIds =
            extractChangedRowIds delta

        -- Check each table used by the query
        hasRelevantChanges =
            List.any
                (\tableName ->
                    case ( Dict.get tableName changedRowIds, Dict.get tableName subscription.resultRowIds ) of
                        ( Just deltaIds, Just resultIds ) ->
                            let
                                -- Find rows that are in both delta and result set
                                overlappingIds =
                                    Set.intersect deltaIds resultIds

                                -- If no overlap, check if new rows might match WHERE clause
                                hasNewRows =
                                    not (Set.isEmpty (Set.diff deltaIds resultIds))
                            in
                            if not (Set.isEmpty overlappingIds) then
                                -- Rows in result set changed - need to check WHERE clause
                                analyzeOverlappingChanges schema db subscription tableName overlappingIds delta

                            else if hasNewRows then
                                -- New rows that aren't in result set - only re-execute if they match WHERE
                                checkIfNewRowsMatchWhere schema db tableName (Set.diff deltaIds resultIds) subscription delta

                            else
                                False

                        ( Just _, Nothing ) ->
                            -- Delta has changes but we have no result rows tracked
                            -- This could be first run or empty result set
                            -- Be conservative and re-execute
                            True

                        ( Nothing, _ ) ->
                            -- This table wasn't changed in the delta
                            False
                )
                queryTables
    in
    if hasRelevantChanges then
        ReExecuteFull

    else
        NoReExecute


{-| Analyze if changes to rows already in the result set require re-execution.

This checks if the query has a WHERE clause and if so, whether the changed
fields are referenced in that clause.

Also handles LIMIT/SORT edge cases.

-}
analyzeOverlappingChanges : Data.Schema.SchemaMetadata -> Db.Db -> QuerySubscription -> String -> Set Int -> Data.Delta.Delta -> Bool
analyzeOverlappingChanges schema db subscription tableName overlappingIds delta =
    let
        -- Get the field query for this table
        maybeFieldQuery =
            Dict.foldl
                (\queryFieldName fieldQuery acc ->
                    case acc of
                        Just _ ->
                            acc

                        Nothing ->
                            case Dict.get queryFieldName schema.queryFieldToTable of
                                Just qTableName ->
                                    if qTableName == tableName then
                                        Just fieldQuery

                                    else
                                        Nothing

                                Nothing ->
                                    Nothing
                )
                Nothing
                subscription.query
    in
    case maybeFieldQuery of
        Just fieldQuery ->
            -- Check for LIMIT/SORT - if present, need to be more conservative
            let
                hasLimitOrSort =
                    fieldQuery.limit /= Nothing || fieldQuery.sort /= Nothing
            in
            if hasLimitOrSort then
                -- With LIMIT/SORT, changes might affect ordering or which rows are in top-N
                -- Check if sorted fields changed
                case fieldQuery.sort of
                    Just sortClauses ->
                        let
                            sortedFields =
                                List.map .field sortClauses |> Set.fromList

                            sortedFieldChanged =
                                checkIfSpecificFieldsChanged db tableName overlappingIds sortedFields delta
                        in
                        -- Re-execute if sorted fields changed
                        sortedFieldChanged

                    Nothing ->
                        -- Has LIMIT but no SORT - changes to existing rows still need re-execution
                        True

            else
                -- No LIMIT/SORT - just check WHERE clause
                case fieldQuery.where_ of
                    Just whereClause ->
                        -- Query has WHERE clause - check if filtered fields changed
                        checkIfFilteredFieldsChanged db tableName overlappingIds whereClause delta

                    Nothing ->
                        -- No WHERE clause - any change to result rows requires re-execution
                        True

        Nothing ->
            -- Shouldn't happen, but be conservative
            True


checkIfNewRowsMatchWhere : Data.Schema.SchemaMetadata -> Db.Db -> String -> Set Int -> QuerySubscription -> Data.Delta.Delta -> Bool
checkIfNewRowsMatchWhere schema db tableName newRowIds subscription delta =
    if Set.isEmpty newRowIds then
        False

    else
        let
            maybeFieldQuery =
                Dict.foldl
                    (\queryFieldName fieldQuery acc ->
                        case acc of
                            Just _ ->
                                acc

                            Nothing ->
                                case Dict.get queryFieldName schema.queryFieldToTable of
                                    Just qTableName ->
                                        if qTableName == tableName then
                                            Just fieldQuery

                                        else
                                            Nothing

                                    Nothing ->
                                        Nothing
                    )
                    Nothing
                    subscription.query

            deltaTableGroup =
                List.filter (\tg -> tg.tableName == tableName) delta.tableGroups
                    |> List.head
        in
        case ( maybeFieldQuery, deltaTableGroup ) of
            ( Just fieldQuery, Just tableGroup ) ->
                let
                    whereClause =
                        fieldQuery.where_
                in
                case whereClause of
                    Just _ ->
                        List.any
                            (\newRowArray ->
                                case newRowArray of
                                    (Data.Value.IntValue rowId) :: _ ->
                                        if Set.member rowId newRowIds then
                                            let
                                                newRow =
                                                    rowArrayToDict tableGroup.headers newRowArray
                                            in
                                            Db.rowMatchesWhere whereClause newRow

                                        else
                                            False

                                    _ ->
                                        False
                            )
                            tableGroup.rows

                    Nothing ->
                        True

            _ ->
                True


{-| Check if any of the changed rows had their filtered fields modified.

Gets old row values from DB, new row values from delta, and compares
fields referenced in the WHERE clause.

-}
checkIfFilteredFieldsChanged : Db.Db -> String -> Set Int -> Db.Query.WhereClause -> Data.Delta.Delta -> Bool
checkIfFilteredFieldsChanged db tableName overlappingIds whereClause delta =
    let
        -- Get the table data from DB (old values)
        oldTableData =
            Dict.get tableName db.tables
                |> Maybe.withDefault Dict.empty

        -- Get the new row data from delta
        deltaTableGroup =
            List.filter (\tg -> tg.tableName == tableName) delta.tableGroups
                |> List.head

        -- For each overlapping row, check if filtered fields changed
        anyFilteredFieldChanged =
            case deltaTableGroup of
                Just tableGroup ->
                    List.any
                        (\newRowArray ->
                            case newRowArray of
                                (Data.Value.IntValue rowId) :: _ ->
                                    if Set.member rowId overlappingIds then
                                        -- This row is in both delta and result set
                                        case Dict.get rowId oldTableData of
                                            Just oldRow ->
                                                let
                                                    newRow =
                                                        rowArrayToDict tableGroup.headers newRowArray
                                                in
                                                doesChangeAffectWhereClause whereClause oldRow newRow

                                            Nothing ->
                                                -- Old row not found - conservative, re-execute
                                                True

                                    else
                                        False

                                _ ->
                                    False
                        )
                        tableGroup.rows

                Nothing ->
                    -- Delta table group not found - shouldn't happen
                    False
    in
    anyFilteredFieldChanged


{-| Convert a row array to a dictionary using headers.
-}
rowArrayToDict : List String -> List Value -> Dict String Value
rowArrayToDict headers values =
    List.map2 Tuple.pair headers values
        |> Dict.fromList


{-| Check if specific fields changed in any of the overlapping rows.

Used for SORT field change detection.

-}
checkIfSpecificFieldsChanged : Db.Db -> String -> Set Int -> Set String -> Data.Delta.Delta -> Bool
checkIfSpecificFieldsChanged db tableName overlappingIds fieldsToCheck delta =
    let
        -- Get the table data from DB (old values)
        oldTableData =
            Dict.get tableName db.tables
                |> Maybe.withDefault Dict.empty

        -- Get the new row data from delta
        deltaTableGroup =
            List.filter (\tg -> tg.tableName == tableName) delta.tableGroups
                |> List.head

        -- For each overlapping row, check if specified fields changed
        anyFieldChanged =
            case deltaTableGroup of
                Just tableGroup ->
                    List.any
                        (\newRowArray ->
                            case newRowArray of
                                (Data.Value.IntValue rowId) :: _ ->
                                    if Set.member rowId overlappingIds then
                                        -- This row is in both delta and result set
                                        case Dict.get rowId oldTableData of
                                            Just oldRow ->
                                                let
                                                    newRow =
                                                        rowArrayToDict tableGroup.headers newRowArray
                                                in
                                                -- Check if any of the specified fields changed
                                                Set.foldl
                                                    (\fieldName hasChanged ->
                                                        if hasChanged then
                                                            True

                                                        else
                                                            Dict.get fieldName oldRow /= Dict.get fieldName newRow
                                                    )
                                                    False
                                                    fieldsToCheck

                                            Nothing ->
                                                -- Old row not found - conservative, re-execute
                                                True

                                    else
                                        False

                                _ ->
                                    False
                        )
                        tableGroup.rows

                Nothing ->
                    -- Delta table group not found
                    False
    in
    anyFieldChanged



-- Ports


port queryManagerOut : Encode.Value -> Cmd msg


port receiveQueryManagerMessage : (Decode.Value -> msg) -> Sub msg


port queryClientOut : Encode.Value -> Cmd msg


port receiveQueryClientMessage : (Decode.Value -> msg) -> Sub msg



-- Encoders


encodeMessage : Message -> Encode.Value
encodeMessage msg =
    case msg of
        QueryResult callbackPort result ->
            Encode.object
                [ ( "type", Encode.string "queryResult" )
                , ( "callbackPort", Encode.string callbackPort )
                , ( "result", result )
                ]

        QueryFull queryId revision result ->
            Encode.object
                [ ( "type", Encode.string "queryFull" )
                , ( "queryId", Encode.string queryId )
                , ( "revision", Encode.int revision )
                , ( "result", result )
                ]

        QueryDelta queryId revision ops ->
            Encode.object
                [ ( "type", Encode.string "queryDelta" )
                , ( "queryId", Encode.string queryId )
                , ( "revision", Encode.int revision )
                , ( "delta"
                  , Encode.object
                        [ ( "ops", Encode.list encodeQueryDeltaOp ops ) ]
                  )
                ]

        MutationResult requestId mutationId result ->
            Encode.object
                [ ( "type", Encode.string "mutationResult" )
                , ( "requestId", Encode.string requestId )
                , ( "mutationId", Encode.string mutationId )
                , ( "result"
                  , case result of
                        Ok value ->
                            Encode.object
                                [ ( "ok", Encode.bool True )
                                , ( "value", value )
                                ]

                        Err error ->
                            Encode.object
                                [ ( "ok", Encode.bool False )
                                , ( "error", Encode.string error )
                                ]
                  )
                ]



-- Decoders


decodeIncoming : Decode.Decoder Incoming
decodeIncoming =
    Decode.field "type" Decode.string
        |> Decode.andThen
            (\type_ ->
                case type_ of
                    "sendMutation" ->
                        Decode.map8 SendMutation
                            (Decode.field "requestId" Decode.string)
                            (Decode.field "mutationId" Decode.string)
                            (Decode.field "baseUrl" Decode.string)
                            (Decode.oneOf
                                [ Decode.field "headers" decodeHeaders
                                , Decode.succeed []
                                ]
                            )
                            (Decode.oneOf
                                [ Decode.field "credentials" Decode.string
                                , Decode.succeed "same-origin"
                                ]
                            )
                            (Decode.oneOf
                                [ Decode.field "withCredentials" Decode.bool
                                , Decode.succeed False
                                ]
                            )
                            (Decode.field "input" Decode.value)
                            (Decode.maybe (Decode.field "optimistic" decodeOptimisticMutation))

                    _ ->
                        Decode.fail ("Unknown QueryManager incoming type: " ++ type_)
            )


decodeOptimisticMutation : Decode.Decoder OptimisticMutation
decodeOptimisticMutation =
    Decode.map3 OptimisticMutation
        (Decode.field "queryField" Decode.string)
        (Decode.field "where" decodeOptimisticWhere)
        (Decode.field "set" (Decode.list decodeOptimisticSetField))


decodeOptimisticWhere : Decode.Decoder OptimisticWhere
decodeOptimisticWhere =
    Decode.map2 OptimisticWhere
        (Decode.field "field" Decode.string)
        (Decode.field "input" Decode.string)


decodeOptimisticSetField : Decode.Decoder OptimisticSetField
decodeOptimisticSetField =
    Decode.map2 OptimisticSetField
        (Decode.field "field" Decode.string)
        (Decode.field "input" Decode.string)


{-| Decoder for QueryClient incoming messages.

The TypeScript QueryClient sends messages with these formats:

  - { "type": "register", "queryId": "...", "querySource": {...}, "queryInput": {...} }
  - { "type": "update-input", "queryId": "...", "querySource": {...}?, "queryInput": {...} }
  - { "type": "unregister", "queryId": "..." }

-}
decodeQueryClientIncoming : Decode.Decoder QueryClientIncoming
decodeQueryClientIncoming =
    Decode.field "type" Decode.string
        |> Decode.andThen
            (\type_ ->
                case type_ of
                    "register" ->
                        Decode.map3 QCRegister
                            (Decode.field "queryId" Decode.string)
                            (Decode.field "querySource" Db.Query.decodeQuery)
                            (Decode.field "queryInput" Decode.value)

                    "update-input" ->
                        Decode.map3 QCUpdateInput
                            (Decode.field "queryId" Decode.string)
                            (Decode.dict Decode.value
                                |> Decode.andThen
                                    (\fields ->
                                        if Dict.member "querySource" fields then
                                            Decode.field "querySource" Db.Query.decodeQuery |> Decode.map Just

                                        else
                                            Decode.succeed Nothing
                                    )
                            )
                            (Decode.field "queryInput" Decode.value)

                    "unregister" ->
                        Decode.field "queryId" Decode.string
                            |> Decode.map QCUnregister

                    _ ->
                        Decode.fail ("Unknown QueryClient incoming type: " ++ type_)
            )



-- Helper functions


decodeHeaders : Decode.Decoder (List ( String, String ))
decodeHeaders =
    Decode.list decodeHeader


decodeHeader : Decode.Decoder ( String, String )
decodeHeader =
    Decode.map2 Tuple.pair
        (Decode.index 0 Decode.string)
        (Decode.index 1 Decode.string)


sendMessage : Message -> Cmd msg
sendMessage msg =
    queryManagerOut (encodeMessage msg)


queryResult : String -> Encode.Value -> Cmd msg
queryResult callbackPort result =
    sendMessage (QueryResult callbackPort result)


queryFull : String -> Int -> Encode.Value -> Cmd msg
queryFull queryId revision result =
    sendMessage (QueryFull queryId revision result)


queryDelta : String -> Int -> List QueryDeltaOp -> Cmd msg
queryDelta queryId revision ops =
    sendMessage (QueryDelta queryId revision ops)


{-| Send a full result to QueryClient via queryClientOut port.

Uses the format expected by TypeScript QueryClientService:
{ "type": "full", "queryId": "...", "revision": 1, "result": {...} }

-}
queryClientFull : String -> Int -> Encode.Value -> Cmd msg
queryClientFull queryId revision result =
    queryClientOut
        (Encode.object
            [ ( "type", Encode.string "full" )
            , ( "queryId", Encode.string queryId )
            , ( "revision", Encode.int revision )
            , ( "result", result )
            ]
        )


{-| Send a delta to QueryClient via queryClientOut port.

Uses the format expected by TypeScript QueryClientService:
{ "type": "delta", "queryId": "...", "revision": 2, "delta": { "ops": [...] } }

-}
queryClientDelta : String -> Int -> List QueryDeltaOp -> Cmd msg
queryClientDelta queryId revision ops =
    queryClientOut
        (Encode.object
            [ ( "type", Encode.string "delta" )
            , ( "queryId", Encode.string queryId )
            , ( "revision", Encode.int revision )
            , ( "delta"
              , Encode.object
                    [ ( "ops", Encode.list encodeQueryDeltaOp ops ) ]
              )
            ]
        )


encodeQueryDeltaOp : QueryDeltaOp -> Encode.Value
encodeQueryDeltaOp op =
    case op of
        SetRow path row ->
            Encode.object
                [ ( "op", Encode.string "set-row" )
                , ( "path", Encode.string path )
                , ( "row", Encode.dict identity Data.Value.encodeValue row )
                ]

        RemoveRow path ->
            Encode.object
                [ ( "op", Encode.string "remove-row" )
                , ( "path", Encode.string path )
                ]

        InsertRow path index row ->
            Encode.object
                [ ( "op", Encode.string "insert-row" )
                , ( "path", Encode.string path )
                , ( "index", Encode.int index )
                , ( "row", Encode.dict identity Data.Value.encodeValue row )
                ]

        MoveRow path from to ->
            Encode.object
                [ ( "op", Encode.string "move-row" )
                , ( "path", Encode.string path )
                , ( "from", Encode.int from )
                , ( "to", Encode.int to )
                ]

        RemoveRowByIndex path index ->
            Encode.object
                [ ( "op", Encode.string "remove-row-by-index" )
                , ( "path", Encode.string path )
                , ( "index", Encode.int index )
                ]


mutationResult : String -> String -> Result String Encode.Value -> Cmd msg
mutationResult requestId mutationId result =
    sendMessage (MutationResult requestId mutationId result)


receiveIncoming : (Result Decode.Error Incoming -> msg) -> Sub msg
receiveIncoming toMsg =
    receiveQueryManagerMessage (\jsonValue -> toMsg (Decode.decodeValue decodeIncoming jsonValue))


receiveQueryClientIncoming : (Result Decode.Error QueryClientIncoming -> msg) -> Sub msg
receiveQueryClientIncoming toMsg =
    receiveQueryClientMessage (\jsonValue -> toMsg (Decode.decodeValue decodeQueryClientIncoming jsonValue))



-- Subscriptions


subscriptions : (Incoming -> msg) -> (String -> msg) -> Sub msg
subscriptions toMsg toErrorMsg =
    receiveIncoming
        (\result ->
            case result of
                Ok incoming ->
                    toMsg incoming

                Err err ->
                    toErrorMsg ("Failed to decode QueryManager message: " ++ Decode.errorToString err)
        )
