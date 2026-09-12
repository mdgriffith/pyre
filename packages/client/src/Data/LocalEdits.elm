module Data.LocalEdits exposing (Model, Pending, State(..), init, receive, visible)

{-| Worker-owned memory-only local edits. Main installs `visible` as its query DB.

Bridge ingress uses the existing receiveQueryManagerMessage port:

    { type: "localEdits", message: Message }

Every Message has the flat fence fields databaseId, instance, authGeneration,
namespace, manifest, databaseEpoch. Identifiers are nonempty strings; generation,
revisions and targets are nonnegative safe integers. Message variants:

  - configure: minimumSafeRevision (authenticated initial security barrier)
  - submit: requestId, operations: [{ operation, input, prediction? }]
  - response: requestId, response (the original, independently fenced server
    accepted/rejected envelope, including its requestId)
  - unknown/cancel: requestId (actual transport uncertainty / unsent cancellation)
  - prepared/notDispatched/preparationFailed: requestId, dispatchId (from prepare).
    prepared authorizes dispatch after host preparation. notDispatched keeps work
    queued and marks connection offline; preparationFailed definitely rejects it.
  - replacement: requestId, target, serverRevision, scope: "database",
    complete: true, tables: { [table]: { rows: [row objects] } }
  - syncRequired: reconciliation: { kind: "replaceRequired", atLeast,
    invalidate?, minimumSafeRevision? }
  - catchupFailed: requestId; retryCatchup: no additional fields
  - connection: connected; dispose/reset: no additional fields

Prediction is trusted manifest-derived bridge metadata, NOT public application
input or a server security boundary. The manifest-aware host validates/captures
inputs and non-predicted result codecs before forwarding. No prediction, null,
or { safe: false } is non-optimistic. A safe prediction has safe: true, kind:
"update" | "create" | "delete", table, id (primitive schema identity), fields?
(captured setters or complete create row), writableFields?, materializedFields?.
Only complete UUID creates qualify; materializedFields is the manifest's complete
field list, not a list inferred from the submitted row. Named operations have no
prediction. Prediction metadata is never included in a network dispatch.

Main emits one queryManagerOut envelope per transition:

    { type: "localEdits", queries: [{ type: "full", queryId, revision, result }],
      events: [Event] }

The host atomically installs queries and the `visible` event before notifying
readers/lifecycle listeners or preparing network work. Events carry flat fences:

  - visible: tables: { [table]: [row objects] }, coveredRevision (-1 if none),
    requiredRevision, invalid. Complete visible scope, including deletions.
  - invalidate: minimumSafeRevision (integer or null), requiredRevision. Evict
    persisted scope/cursor/coverage together; null requires a security barrier.
  - lifecycle: requestId, state, optional sequence/commitRevision/results/code.
    States: queued, locallyApplied, sent, accepted, confirmed, rejected,
    outcomeUnknown, acceptedUnreconciled.
  - failure: requestId, phase, code, certainty, optional operationIndex.
  - quarantined: requestId. Separate from outcome; never a failure.
  - replacementInstalled: requestId, serverRevision, scope, complete, tables
    (same flattened table arrays as visible). Persist ONLY this complete base
    and coverage/epoch together, never visible overlays or separate revisions.
  - prepare/dispatch: requestId, dispatchId, sequence, version: 1,
    operations: [{ operation, input }]. Prepare reserves the single slot before
    async token lookup/encoding; it does NOT authorize network I/O. Reply prepared
    with dispatchId; only the subsequent dispatch authorizes sending. Use configured transport;
    send only the version/fence/requestId/sequence/operations network envelope.
  - catchup: requestId, target. Use configured authenticated read transport;
    forward the complete server replacement unchanged. Do not synthesize pages.
  - reconciliationFailure: requestId (read), phase, code. Accepted receipt failures
    are also emitted through failure with certainty acceptedUnreconciled.
  - lifetimeEnded: deliver preceding final events before detaching the old fence.
  - preparationCancelled: requestId, dispatchId. Discard that prepared transport.

Consume envelopes and effects in order. Never retry a dispatch automatically.
Send connection:false before enqueue when offline; a transport failure after
dispatch authorization must produce unknown, not a made-up server rejection. A new
configure is an explicit recovery choice, not an ordering barrier for old work.
Old lifetimes cannot be reopened. Legacy unfenced cache/live messages are not
coverage and Main ignores them once this bridge is activated.

-}

import Data.Identity as Identity
import Data.Schema as Schema
import Data.Value as Value exposing (Value)
import Db
import Db.Index
import Dict exposing (Dict)
import Json.Decode as D
import Json.Encode as E
import Set exposing (Set)


type alias Fence =
    { databaseId : String, instance : String, authGeneration : Int, namespace : String, manifest : String, databaseEpoch : String }


type State
    = Queued
    | Preparing Int
    | Sent
    | Unknown
    | Accepted Int E.Value


type alias Operation =
    { operation : String, input : E.Value, prediction : Maybe Intent }


type alias Intent =
    { kind : String, table : String, id : Value, fields : Dict String Value, writableFields : List String, materializedFields : List String }


type alias Pending =
    { requestId : String, sequence : Int, operations : List Operation, state : State, quarantined : Bool }


type alias Model =
    { authoritative : Db.Db
    , fence : Maybe Fence
    , retiredFences : Set String
    , pending : List Pending
    , seen : Set String
    , sequence : Int
    , dispatchSequence : Int
    , coveredRevision : Int
    , requiredRevision : Int
    , minimumSafeRevision : Maybe Int
    , securityFloor : Int
    , securityBarrierRequired : Bool
    , invalid : Bool
    , connected : Bool
    , catchup : Maybe ( String, Int )
    , catchupSequence : Int
    , catchupFailed : Bool
    , catchupErrorReported : Bool
    }


init : Schema.SchemaMetadata -> Model
init schema =
    { authoritative = Db.init schema, fence = Nothing, retiredFences = Set.empty, pending = [], seen = Set.empty, sequence = 0, dispatchSequence = 0, coveredRevision = -1, requiredRevision = 0, minimumSafeRevision = Nothing, securityFloor = 0, securityBarrierRequired = False, invalid = True, connected = True, catchup = Nothing, catchupSequence = 0, catchupFailed = False, catchupErrorReported = False }


visible : Model -> Db.Db
visible model =
    if model.invalid then
        Db.init model.authoritative.schema

    else
        List.foldl
            (\pending db ->
                if pending.quarantined then
                    db

                else
                    replay pending.operations db |> Maybe.withDefault db
            )
            model.authoritative
            model.pending


{-| A failed member discards the entire simulated batch, including earlier creates
and deletes. Updates store only setters, never a forward or inverse row snapshot.
-}
replay : List Operation -> Db.Db -> Maybe Db.Db
replay operations db =
    List.foldl
        (\operation result ->
            result |> Maybe.andThen (\current -> operation.prediction |> Maybe.andThen (applyIntent current))
        )
        (Just db)
        operations


applyIntent : Db.Db -> Intent -> Maybe Db.Db
applyIntent db intent =
    Dict.get intent.table db.schema.tables
        |> Maybe.andThen
            (\metadata ->
                Identity.fromValue metadata.primaryKey.kind intent.id
                    |> Result.toMaybe
                    |> Maybe.andThen
                        (\key ->
                            let
                                table =
                                    Dict.get intent.table db.tables |> Maybe.withDefault Dict.empty

                                existing =
                                    Dict.get key table

                                writable =
                                    List.all (\field -> field /= metadata.primaryKey.name && List.member field intent.writableFields) (Dict.keys intent.fields)

                                nextTable =
                                    case intent.kind of
                                        "update" ->
                                            if writable && not (Dict.isEmpty intent.fields) then
                                                existing
                                                    |> Maybe.andThen
                                                        (\row ->
                                                            if List.all (\field -> Dict.member field row) (Dict.keys intent.fields) then
                                                                Just (Dict.insert key (Dict.union intent.fields row) table)

                                                            else
                                                                Nothing
                                                        )

                                            else
                                                Nothing

                                        "create" ->
                                            if metadata.primaryKey.kind == Schema.UuidKey && existing == Nothing && Dict.get metadata.primaryKey.name intent.fields == Just intent.id && Set.fromList (Dict.keys intent.fields) == Set.fromList intent.materializedFields then
                                                Just (Dict.insert key intent.fields table)

                                            else
                                                Nothing

                                        "delete" ->
                                            existing |> Maybe.map (\_ -> Dict.remove key table)

                                        _ ->
                                            Nothing
                            in
                            nextTable
                                |> Maybe.map
                                    (\next ->
                                        let
                                            tables =
                                                Dict.insert intent.table next db.tables
                                        in
                                        { db | tables = tables, indices = Db.Index.buildIndicesFromSchema db.schema tables }
                                    )
                        )
            )


{-| Only this bridge enters the fenced lifetime. Legacy cache pages and live deltas
must never be passed to this reducer as authoritative coverage.
-}
receive : E.Value -> Model -> ( Model, List E.Value )
receive value model =
    let
        -- Decode.value otherwise retains the host's mutable JS object for inputs
        -- and results. Capture the JSON at ingress, not at eventual dispatch.
        captured =
            D.decodeString D.value (E.encode 0 value) |> Result.withDefault E.null

        ( next, events ) =
            receiveInternal captured model

        invalidations =
            if next.fence /= Nothing && next.invalid && (next.fence /= model.fence || not model.invalid || next.minimumSafeRevision /= model.minimumSafeRevision) then
                [ event next "invalidate" [ ( "minimumSafeRevision", next.minimumSafeRevision |> Maybe.map E.int |> Maybe.withDefault E.null ), ( "requiredRevision", E.int next.requiredRevision ) ] ]

            else
                []
    in
    ( next
    , invalidations
        ++ (if (visible next).tables == (visible model).tables && next.invalid == model.invalid && next.fence == model.fence then
                List.filter (\item -> D.decodeValue (D.field "type" D.string) item /= Ok "visible") events

            else
                events
           )
    )


receiveInternal : E.Value -> Model -> ( Model, List E.Value )
receiveInternal value model =
    case D.decodeValue (D.field "type" D.string) value of
        Err _ ->
            ( model, [] )

        Ok "configure" ->
            case D.decodeValue (D.map2 Tuple.pair fenceDecoder (D.field "minimumSafeRevision" revisionDecoder)) value of
                Ok ( fence, minimum ) ->
                    if model.fence == Just fence || Set.member (E.encode 0 (E.object (fenceFields (Just fence)))) model.retiredFences then
                        ( model, [] )

                    else
                        let
                            ( fresh, endings ) =
                                dispose model
                        in
                        finish { fresh | fence = Just fence, minimumSafeRevision = Just minimum, securityFloor = minimum, requiredRevision = minimum } []
                            |> Tuple.mapSecond ((++) endings)

                Err _ ->
                    ( model, [] )

        Ok kind ->
            if model.fence == Nothing || (D.decodeValue fenceDecoder value |> Result.toMaybe) /= model.fence then
                ( model, [] )

            else
                transition kind value model |> (\( next, events ) -> finish next events)


transition : String -> E.Value -> Model -> ( Model, List E.Value )
transition kind value model =
    let
        requestId =
            D.decodeValue (D.field "requestId" D.string) value |> Result.withDefault ""

        findPending =
            List.filter (\p -> p.requestId == requestId) model.pending |> List.head

        change fn =
            { model
                | pending =
                    List.map
                        (\p ->
                            if p.requestId == requestId then
                                fn p

                            else
                                p
                        )
                        model.pending
            }

        remove =
            { model | pending = List.filter (\p -> p.requestId /= requestId) model.pending }
    in
    if List.member kind [ "prepared", "notDispatched", "preparationFailed" ] then
        case findPending |> Maybe.map .state of
            Just (Preparing dispatchId) ->
                if D.decodeValue (D.field "dispatchId" revisionDecoder) value /= Ok dispatchId then
                    ( model, [] )

                else if kind == "preparationFailed" then
                    ( remove, [ lifecycle model requestId "rejected" [], failure model requestId "preparation" "PreparationFailed" "rejected" ] )

                else if kind == "notDispatched" || not model.connected || model.invalid then
                    let
                        next =
                            change (\p -> { p | state = Queued })
                    in
                    ( { next | connected = next.connected && kind /= "notDispatched" }
                    , [ event model "preparationCancelled" [ ( "requestId", E.string requestId ), ( "dispatchId", E.int dispatchId ) ] ]
                    )

                else
                    case findPending of
                        Just p ->
                            ( change (\item -> { item | state = Sent })
                            , [ lifecycle model requestId "sent" [], transportEvent model "dispatch" dispatchId p ]
                            )

                        Nothing ->
                            ( model, [] )

            _ ->
                ( model, [] )

    else
        case kind of
            "submit" ->
                if String.isEmpty requestId || Set.member requestId model.seen then
                    ( model, [] )

                else
                    let
                        reserved =
                            { model | seen = Set.insert requestId model.seen, sequence = model.sequence + 1 }

                        invalid =
                            ( reserved, [ lifecycle reserved requestId "rejected" [], failure reserved requestId "validation" "InvalidEdit" "rejected" ] )
                    in
                    case D.decodeValue (D.field "operations" (D.list operationDecoder)) value of
                        Err _ ->
                            invalid

                        Ok [] ->
                            ( { reserved | sequence = model.sequence }, [ lifecycle reserved requestId "confirmed" [ ( "results", E.list identity [] ) ] ] )

                        Ok operations ->
                            if List.any (invalidOperation model.authoritative.schema) operations then
                                invalid

                            else
                                let
                                    pending =
                                        { requestId = requestId, sequence = reserved.sequence, operations = operations, state = Queued, quarantined = False }

                                    applied =
                                        not model.invalid && replay operations (visible model) /= Nothing

                                    next =
                                        { reserved | pending = reserved.pending ++ [ pending ] }
                                in
                                ( next
                                , [ lifecycle next
                                        requestId
                                        (if applied then
                                            "locallyApplied"

                                         else
                                            "queued"
                                        )
                                        [ ( "sequence", E.int pending.sequence ) ]
                                  ]
                                )

            "response" ->
                case findPending of
                    Just pending ->
                        if isUnsent pending.state then
                            ( model, [] )

                        else
                            case D.decodeValue (D.field "response" D.value) value of
                                Err _ ->
                                    unknown pending "MalformedResponse" model

                                Ok response ->
                                    if (D.decodeValue fenceDecoder response |> Result.toMaybe) == Nothing || (D.decodeValue (D.field "requestId" nonempty) response |> Result.toMaybe) == Nothing then
                                        unknown pending "MalformedResponse" model

                                    else if D.decodeValue fenceDecoder response /= D.decodeValue fenceDecoder value || D.decodeValue (D.field "requestId" D.string) response /= Ok requestId then
                                        ( model, [] )

                                    else
                                        case D.decodeValue (D.field "status" D.string) response of
                                            Ok "accepted" ->
                                                case D.decodeValue (D.map3 (\revision results hint -> ( revision, results, hint )) (D.field "commitRevision" revisionDecoder) (D.field "results" (D.list resultDecoder)) (D.field "reconciliation" hintDecoder)) response of
                                                    Ok ( revision, results, hint ) ->
                                                        if List.length results /= List.length pending.operations || not (List.all identity (List.map2 (validResult model.authoritative.schema) results (List.indexedMap Tuple.pair pending.operations))) || hint.atLeast < revision then
                                                            unknown pending "MalformedResponse" (applyHint hint model)

                                                        else
                                                            case pending.state of
                                                                Accepted _ _ ->
                                                                    ( model, [] )

                                                                _ ->
                                                                    let
                                                                        resultValue =
                                                                            D.decodeValue (D.field "results" D.value) response |> Result.withDefault E.null

                                                                        next =
                                                                            change (\p -> { p | state = Accepted revision resultValue, quarantined = False }) |> applyHint hint
                                                                    in
                                                                    ( next, [ lifecycle next requestId "accepted" [ ( "commitRevision", E.int revision ), ( "results", resultValue ) ] ] )

                                                    Err _ ->
                                                        unknown pending "MalformedResponse" (invalidateUnknown response model)

                                            Ok "rejected" ->
                                                if List.any (\name -> D.decodeValue (D.field name D.value) response |> Result.toMaybe |> (/=) Nothing) [ "commitRevision", "results", "reconciliation" ] then
                                                    unknown pending "MalformedResponse" (invalidateUnknown response model)

                                                else
                                                    case ( pending.state, D.decodeValue (D.field "code" D.string) response ) of
                                                        ( Accepted _ _, _ ) ->
                                                            ( model, [] )

                                                        ( _, Ok code ) ->
                                                            let
                                                                safeCode =
                                                                    if List.member code [ "InvalidEdit", "TargetNotWritable", "InvalidBatch", "InvalidOperation", "ManifestMismatch", "NamespaceMismatch", "NotAuthenticated", "PermissionDenied", "TransactionFailed" ] then
                                                                        code

                                                                    else
                                                                        "Rejected"

                                                                indexFields =
                                                                    case D.decodeValue (D.field "operationIndex" revisionDecoder) response of
                                                                        Ok index ->
                                                                            if index < List.length pending.operations then
                                                                                [ ( "operationIndex", E.int index ) ]

                                                                            else
                                                                                []

                                                                        Err _ ->
                                                                            []
                                                            in
                                                            ( remove
                                                            , [ lifecycle model requestId "rejected" [ ( "code", E.string safeCode ) ]
                                                              , event model "failure" ([ ( "requestId", E.string requestId ), ( "phase", E.string "server" ), ( "code", E.string safeCode ), ( "certainty", E.string "rejected" ) ] ++ indexFields)
                                                              ]
                                                            )

                                                        _ ->
                                                            unknown pending "MalformedResponse" model

                                            _ ->
                                                unknown pending "MalformedResponse" model

                    Nothing ->
                        ( model, [] )

            "unknown" ->
                case findPending of
                    Just pending ->
                        unknown pending "TransportFailure" model

                    Nothing ->
                        ( model, [] )

            "cancel" ->
                case findPending of
                    Just pending ->
                        if isUnsent pending.state then
                            ( remove, [ lifecycle model requestId "rejected" [], failure model requestId "dispatch" "Cancelled" "rejected" ] )

                        else
                            ( model, [] )

                    Nothing ->
                        ( model, [] )

            "syncRequired" ->
                case D.decodeValue (D.field "reconciliation" hintDecoder) value of
                    Ok hint ->
                        ( applyHint hint model, [] )

                    Err _ ->
                        ( invalidateUnknown value model, [] )

            "replacement" ->
                case ( model.catchup, D.decodeValue replacementDecoder value ) of
                    ( Just ( expectedId, target ), Ok replacement ) ->
                        if requestId /= expectedId then
                            ( model, [] )

                        else if replacement.target /= target || replacement.revision < target || replacement.revision < model.coveredRevision || Dict.keys replacement.tables /= Dict.keys model.authoritative.schema.tables then
                            catchupFailure model

                        else if model.minimumSafeRevision |> Maybe.map (\minimum -> replacement.revision < minimum) |> Maybe.withDefault True then
                            -- Security hints can overtake a valid captured read. Discard
                            -- that read, then capture a new target rather than waiting on it.
                            ( { model | catchup = Nothing }, [] )

                        else
                            case Db.fromInitialData model.authoritative.schema { tables = replacement.tables, cursor = Dict.empty, lastAppliedServerRevision = Just replacement.revision, databaseEpoch = model.fence |> Maybe.map .databaseEpoch } of
                                Err _ ->
                                    catchupFailure model

                                Ok base ->
                                    let
                                        next =
                                            { model
                                                | authoritative = base
                                                , coveredRevision = replacement.revision
                                                , invalid = False
                                                , catchup = Nothing
                                                , catchupFailed = False
                                                , catchupErrorReported = False
                                                , pending = List.map (\p -> { p | quarantined = p.quarantined || p.state == Sent || p.state == Unknown }) model.pending
                                            }
                                    in
                                    ( next
                                    , event next "replacementInstalled" [ ( "requestId", E.string requestId ), ( "serverRevision", E.int replacement.revision ), ( "scope", E.string "database" ), ( "complete", E.bool True ), ( "tables", encodeTables base ) ]
                                        :: (model.pending
                                                |> List.filter (\p -> not p.quarantined && (p.state == Sent || p.state == Unknown))
                                                |> List.map (\p -> event next "quarantined" [ ( "requestId", E.string p.requestId ), ( "quarantined", E.bool True ) ])
                                           )
                                    )

                    ( Just ( expectedId, _ ), Err _ ) ->
                        if requestId == expectedId then
                            catchupFailure model

                        else
                            ( model, [] )

                    _ ->
                        ( model, [] )

            "catchupFailed" ->
                if model.catchup |> Maybe.map (Tuple.first >> (==) requestId) |> Maybe.withDefault False then
                    catchupFailure model

                else
                    ( model, [] )

            "retryCatchup" ->
                ( { model | catchupFailed = False }, [] )

            "connection" ->
                case D.decodeValue (D.field "connected" D.bool) value of
                    Ok connected ->
                        ( { model | connected = connected }, [] )

                    Err _ ->
                        ( model, [] )

            "dispose" ->
                dispose model

            "reset" ->
                dispose model

            _ ->
                ( model, [] )


invalidOperation : Schema.SchemaMetadata -> Operation -> Bool
invalidOperation schema operation =
    String.isEmpty operation.operation
        || (case operation.prediction of
                Just intent ->
                    case Dict.get intent.table schema.tables of
                        Nothing ->
                            True

                        Just metadata ->
                            (Identity.fromValue metadata.primaryKey.kind intent.id |> Result.toMaybe)
                                == Nothing
                                || not (List.member intent.kind [ "update", "create", "delete" ])
                                || (intent.kind == "update" && (Dict.isEmpty intent.fields || List.any (\field -> field == metadata.primaryKey.name || not (List.member field intent.writableFields)) (Dict.keys intent.fields)))

                Nothing ->
                    False
           )


isUnsent : State -> Bool
isUnsent state =
    case state of
        Queued ->
            True

        Preparing _ ->
            True

        _ ->
            False


validResult : Schema.SchemaMetadata -> ( Int, String, E.Value ) -> ( Int, Operation ) -> Bool
validResult schema ( index, operation, value ) ( expectedIndex, expected ) =
    index
        == expectedIndex
        && operation
        == expected.operation
        && (case expected.prediction of
                Nothing ->
                    -- Named and non-predicted operation codecs belong to the
                    -- manifest-aware host. Never infer a codec from an ID/name.
                    True

                Just intent ->
                    case ( Dict.get intent.table schema.tables, D.decodeValue (D.field "id" Value.decodeValue) value ) of
                        ( Just metadata, Ok id ) ->
                            (Identity.fromValue metadata.primaryKey.kind id |> Result.toMaybe) /= Nothing && id == intent.id

                        _ ->
                            False
           )


unknown : Pending -> String -> Model -> ( Model, List E.Value )
unknown pending code model =
    case pending.state of
        Sent ->
            ( { model
                | pending =
                    List.map
                        (\p ->
                            if p.requestId == pending.requestId then
                                { p | state = Unknown }

                            else
                                p
                        )
                        model.pending
              }
            , [ lifecycle model pending.requestId "outcomeUnknown" [ ( "code", E.string code ) ], failure model pending.requestId "transport" code "unknown" ]
            )

        _ ->
            ( model, [] )


dispose : Model -> ( Model, List E.Value )
dispose model =
    let
        fresh =
            init model.authoritative.schema
    in
    ( { fresh | retiredFences = Set.insert (E.encode 0 (E.object (fenceFields model.fence))) model.retiredFences }
    , (if model.fence == Nothing then
        []

       else
        [ event model "visible" [ ( "tables", E.object [] ), ( "coveredRevision", E.int -1 ), ( "requiredRevision", E.int 0 ), ( "invalid", E.bool True ) ] ]
      )
        ++ List.concatMap
            (\p ->
                let
                    ( state, certainty, fields ) =
                        case p.state of
                            Queued ->
                                ( "rejected", "rejected", [] )

                            Preparing _ ->
                                ( "rejected", "rejected", [] )

                            Accepted revision results ->
                                ( "acceptedUnreconciled", "acceptedUnreconciled", [ ( "commitRevision", E.int revision ), ( "results", results ) ] )

                            _ ->
                                ( "outcomeUnknown", "unknown", [] )
                in
                if p.state == Unknown then
                    []

                else
                    [ lifecycle model p.requestId state fields, failure model p.requestId "lifetime" "Fenced" certainty ]
            )
            model.pending
        ++ (if model.fence == Nothing then
                []

            else
                [ event model "lifetimeEnded" [] ]
           )
    )


catchupFailure : Model -> ( Model, List E.Value )
catchupFailure model =
    ( { model | catchup = Nothing, catchupFailed = True, catchupErrorReported = True }
    , if model.catchupErrorReported then
        []

      else
        event model "reconciliationFailure" [ ( "requestId", E.string (model.catchup |> Maybe.map Tuple.first |> Maybe.withDefault "") ), ( "phase", E.string "reconciliation" ), ( "code", E.string "CatchupFailed" ) ]
            :: List.filterMap
                (\p ->
                    case p.state of
                        Accepted _ _ ->
                            Just (failure model p.requestId "reconciliation" "CatchupFailed" "acceptedUnreconciled")

                        _ ->
                            Nothing
                )
                model.pending
    )


type alias Hint =
    { atLeast : Int, invalidate : Bool, minimum : Maybe Int }


invalidateUnknown : E.Value -> Model -> Model
invalidateUnknown value model =
    let
        knownRevision =
            [ D.field "serverRevision" revisionDecoder, D.field "commitRevision" revisionDecoder, D.at [ "reconciliation", "atLeast" ] revisionDecoder ]
                |> List.filterMap (\decoder -> D.decodeValue decoder value |> Result.toMaybe)
                |> List.maximum

        revision =
            Maybe.withDefault 0 knownRevision
    in
    { model | invalid = True, minimumSafeRevision = Nothing, securityFloor = max revision model.securityFloor, securityBarrierRequired = model.securityBarrierRequired || knownRevision == Nothing, requiredRevision = max revision model.requiredRevision }


applyHint : Hint -> Model -> Model
applyHint hint model =
    { model
        | requiredRevision = max model.requiredRevision hint.atLeast
        , invalid = model.invalid || hint.invalidate
        , minimumSafeRevision =
            if model.securityBarrierRequired then
                -- Without a revision bound, an uncorrelated hint could predate the
                -- uncertainty. Recovery requires a new authenticated lifetime.
                Nothing

            else if hint.invalidate then
                hint.minimum
                    |> Maybe.andThen
                        (\minimum ->
                            if minimum >= hint.atLeast then
                                if minimum >= model.securityFloor then
                                    Just minimum

                                else
                                    -- A stale barrier cannot establish safety for newer uncertainty.
                                    model.minimumSafeRevision

                            else
                                Nothing
                        )

            else
                model.minimumSafeRevision
        , securityFloor =
            if hint.invalidate then
                max model.securityFloor (max hint.atLeast (Maybe.withDefault 0 hint.minimum))

            else
                model.securityFloor
    }


finish : Model -> List E.Value -> ( Model, List E.Value )
finish model events =
    let
        confirmed p =
            case p.state of
                Accepted revision _ ->
                    not model.invalid && model.coveredRevision >= revision

                _ ->
                    False

        confirmations =
            List.filter confirmed model.pending
                |> List.map
                    (\p ->
                        case p.state of
                            Accepted revision results ->
                                lifecycle model p.requestId "confirmed" [ ( "commitRevision", E.int revision ), ( "results", results ) ]

                            _ ->
                                E.null
                    )

        retired =
            { model | pending = List.filter (confirmed >> not) model.pending }

        unresolved =
            List.any
                (\p ->
                    p.state
                        /= Queued
                        && (case p.state of
                                Accepted _ _ ->
                                    False

                                _ ->
                                    True
                           )
                )
                retired.pending

        toDispatch =
            if retired.connected && not retired.invalid && not unresolved then
                List.filter (\p -> p.state == Queued) retired.pending |> List.head

            else
                Nothing

        ( dispatched, dispatchEvents ) =
            case toDispatch of
                Nothing ->
                    ( retired, [] )

                Just p ->
                    ( { retired
                        | pending =
                            List.map
                                (\item ->
                                    if item.requestId == p.requestId then
                                        { item | state = Preparing (retired.dispatchSequence + 1) }

                                    else
                                        item
                                )
                                retired.pending
                        , dispatchSequence = retired.dispatchSequence + 1
                      }
                    , [ transportEvent retired "prepare" (retired.dispatchSequence + 1) p ]
                    )

        ( next, catchupEvents ) =
            if dispatched.fence /= Nothing && dispatched.connected && dispatched.catchup == Nothing && not dispatched.catchupFailed && (dispatched.invalid || dispatched.coveredRevision < dispatched.requiredRevision) && dispatched.minimumSafeRevision /= Nothing then
                let
                    target =
                        max dispatched.requiredRevision (max dispatched.coveredRevision (Maybe.withDefault 0 dispatched.minimumSafeRevision))

                    requestId =
                        "$catchup-" ++ String.fromInt (dispatched.catchupSequence + 1)
                in
                ( { dispatched | catchup = Just ( requestId, target ), catchupSequence = dispatched.catchupSequence + 1 }
                , [ event dispatched "catchup" [ ( "requestId", E.string requestId ), ( "target", E.int target ) ] ]
                )

            else
                ( dispatched, [] )

        publication =
            event next "visible" [ ( "tables", encodeTables (visible next) ), ( "coveredRevision", E.int next.coveredRevision ), ( "requiredRevision", E.int next.requiredRevision ), ( "invalid", E.bool next.invalid ) ]
    in
    ( next
    , (if next.fence == Nothing then
        []

       else
        [ publication ]
      )
        ++ events
        ++ confirmations
        ++ dispatchEvents
        ++ catchupEvents
    )


encodeTables : Db.Db -> E.Value
encodeTables db =
    E.dict identity (Dict.values >> E.list (E.dict identity Value.encodeValue)) db.tables


transportEvent : Model -> String -> Int -> Pending -> E.Value
transportEvent model kind dispatchId pending =
    event model kind [ ( "requestId", E.string pending.requestId ), ( "dispatchId", E.int dispatchId ), ( "sequence", E.int pending.sequence ), ( "version", E.int 1 ), ( "operations", E.list (\op -> E.object [ ( "operation", E.string op.operation ), ( "input", op.input ) ]) pending.operations ) ]


event : Model -> String -> List ( String, E.Value ) -> E.Value
event model kind fields =
    E.object (( "type", E.string kind ) :: (fenceFields model.fence ++ fields))


lifecycle : Model -> String -> String -> List ( String, E.Value ) -> E.Value
lifecycle model requestId state fields =
    let
        quarantined =
            not (List.member state [ "accepted", "confirmed", "rejected", "acceptedUnreconciled" ])
                && (List.filter (\p -> p.requestId == requestId) model.pending |> List.head |> Maybe.map .quarantined |> Maybe.withDefault False)
    in
    event model "lifecycle" (( "requestId", E.string requestId ) :: ( "state", E.string state ) :: ( "quarantined", E.bool quarantined ) :: fields)


failure : Model -> String -> String -> String -> String -> E.Value
failure model requestId phase code certainty =
    event model "failure" [ ( "requestId", E.string requestId ), ( "phase", E.string phase ), ( "code", E.string code ), ( "certainty", E.string certainty ) ]


fenceFields : Maybe Fence -> List ( String, E.Value )
fenceFields maybeFence =
    case maybeFence of
        Nothing ->
            []

        Just fence ->
            [ ( "databaseId", E.string fence.databaseId ), ( "instance", E.string fence.instance ), ( "authGeneration", E.int fence.authGeneration ), ( "namespace", E.string fence.namespace ), ( "manifest", E.string fence.manifest ), ( "databaseEpoch", E.string fence.databaseEpoch ) ]


nonempty : D.Decoder String
nonempty =
    D.string
        |> D.andThen
            (\text ->
                if String.isEmpty text then
                    D.fail "Empty identifier"

                else
                    D.succeed text
            )


revisionDecoder : D.Decoder Int
revisionDecoder =
    D.int
        |> D.andThen
            (\number ->
                if number >= 0 && number <= 9007199254740991 then
                    D.succeed number

                else
                    D.fail "Invalid revision"
            )


fenceDecoder : D.Decoder Fence
fenceDecoder =
    D.map6 Fence (D.field "databaseId" nonempty) (D.field "instance" nonempty) (D.field "authGeneration" revisionDecoder) (D.field "namespace" nonempty) (D.field "manifest" nonempty) (D.field "databaseEpoch" nonempty)


optional : String -> D.Decoder a -> a -> D.Decoder a
optional field decoder default =
    D.dict D.value
        |> D.andThen
            (\fields ->
                if Dict.member field fields then
                    D.field field decoder

                else
                    D.succeed default
            )


operationDecoder : D.Decoder Operation
operationDecoder =
    D.map3 Operation
        (D.field "operation" nonempty)
        (D.field "input" D.value)
        (optional "prediction"
            (D.nullable
                (D.field "safe" D.bool
                    |> D.andThen
                        (\safe ->
                            if safe then
                                D.map Just intentDecoder

                            else
                                D.succeed Nothing
                        )
                )
                |> D.map (Maybe.andThen identity)
            )
            Nothing
        )


intentDecoder : D.Decoder Intent
intentDecoder =
    D.field "safe" D.bool
        |> D.andThen
            (\safe ->
                if safe then
                    D.map6 Intent (D.field "kind" nonempty) (D.field "table" nonempty) (D.field "id" Value.decodeValue) (optional "fields" (D.dict Value.decodeValue) Dict.empty) (optional "writableFields" (D.list nonempty) []) (optional "materializedFields" (D.list nonempty) [])

                else
                    D.fail "Prediction must be explicitly proven safe"
            )


resultDecoder : D.Decoder ( Int, String, E.Value )
resultDecoder =
    D.map3 (\index operation value -> ( index, operation, value )) (D.field "index" revisionDecoder) (D.field "operation" nonempty) (D.field "value" D.value)


hintDecoder : D.Decoder Hint
hintDecoder =
    D.field "kind" D.string
        |> D.andThen
            (\kind ->
                if kind == "replaceRequired" then
                    D.map3 Hint (D.field "atLeast" revisionDecoder) (optional "invalidate" D.bool True) (optional "minimumSafeRevision" (D.map Just revisionDecoder) Nothing)

                else
                    D.fail "Expected replaceRequired"
            )


type alias Replacement =
    { target : Int, revision : Int, tables : Dict String (List (Dict String Value)) }


replacementDecoder : D.Decoder Replacement
replacementDecoder =
    D.map2 Tuple.pair (D.field "scope" D.string) (D.field "complete" D.bool)
        |> D.andThen
            (\( scope, complete ) ->
                if scope == "database" && complete then
                    D.map3 Replacement (D.field "target" revisionDecoder) (D.field "serverRevision" revisionDecoder) (D.field "tables" (D.dict (D.field "rows" (D.list (D.dict Value.decodeValue)))))

                else
                    D.fail "Expected complete database replacement"
            )
