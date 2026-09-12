module Pyre.LocalEdits exposing (EditFailure, Lifecycle, Model, Outcome(..), Receipt, clearFailures, failures, init, lifecycle, outcome, receive, state, submit)

import Db.Database as Database
import Dict exposing (Dict)
import Json.Decode as D
import Json.Encode as E
import Pyre.Edit.Internal as Internal


type Model
    = Model String Int (Dict String Entry) (List EditFailure)


type alias Entry =
    { databaseId : String, validate : D.Decoder (), state : String, results : Maybe E.Value, code : String, sequence : Maybe Int, commitRevision : Maybe Int, quarantined : Bool }


type alias Lifecycle =
    { state : String, sequence : Maybe Int, commitRevision : Maybe Int, quarantined : Bool }


type Receipt a
    = Receipt String (D.Decoder a)


type Outcome a
    = Confirmed a
    | Rejected String
    | OutcomeUnknown String
    | AcceptedUnreconciled a


type alias EditFailure =
    { requestId : String, databaseId : String, instance : String, authGeneration : Int, namespace : String, manifest : String, databaseEpoch : String, phase : String, code : String, certainty : String, operationIndex : Maybe Int }


init : String -> Model
init incarnation =
    Model incarnation 0 Dict.empty []


submit : Database.DatabaseId n -> Internal.Batch n a -> Model -> ( Model, Maybe E.Value, Receipt a )
submit database plan (Model incarnation counter entries _) =
    let
        requestId =
            "elm:" ++ incarnation ++ ":" ++ String.fromInt (counter + 1)

        decode =
            Internal.decoder plan

        empty =
            List.isEmpty (Internal.operations plan)

        entry =
            { databaseId = Database.toString database
            , validate = D.map (always ()) decode
            , state =
                if empty then
                    "confirmed"

                else
                    "queued"
            , results =
                if empty then
                    Just (E.list identity [])

                else
                    Nothing
            , code = ""
            , sequence = Nothing
            , commitRevision = Nothing
            , quarantined = False
            }

        wire =
            E.object [ ( "type", E.string "elm-local-edits" ), ( "databaseId", Database.encode database ), ( "requestId", E.string requestId ), ( "operations", E.list identity (Internal.operations plan) ) ]
    in
    ( Model incarnation (counter + 1) (Dict.insert requestId entry entries) []
    , if empty then
        Nothing

      else
        Just wire
    , Receipt requestId decode
    )


state : Receipt a -> Model -> Maybe String
state (Receipt requestId _) (Model _ _ entries _) =
    Dict.get requestId entries |> Maybe.map .state


lifecycle : Receipt a -> Model -> Maybe Lifecycle
lifecycle (Receipt requestId _) (Model _ _ entries _) =
    Dict.get requestId entries
        |> Maybe.map (\entry -> { state = entry.state, sequence = entry.sequence, commitRevision = entry.commitRevision, quarantined = entry.quarantined })


outcome : Receipt a -> Model -> Maybe (Outcome a)
outcome (Receipt requestId decode) (Model _ _ entries _) =
    Dict.get requestId entries
        |> Maybe.andThen
            (\entry ->
                case entry.state of
                    "confirmed" ->
                        entry.results |> Maybe.andThen (D.decodeValue decode >> Result.toMaybe) |> Maybe.map Confirmed

                    "acceptedUnreconciled" ->
                        entry.results |> Maybe.andThen (D.decodeValue decode >> Result.toMaybe) |> Maybe.map AcceptedUnreconciled

                    "rejected" ->
                        Just (Rejected entry.code)

                    "outcomeUnknown" ->
                        Just (OutcomeUnknown entry.code)

                    _ ->
                        Nothing
            )


failures : Model -> List EditFailure
failures (Model _ _ _ events) =
    events


clearFailures : Model -> Model
clearFailures (Model incarnation counter entries _) =
    Model incarnation counter entries []


{-| Only lifecycle/failure envelopes from the manifest-aware host belong here.
The host fences worker traffic; this model never receives or caches rows.
-}
receive : E.Value -> Model -> Model
receive value (Model incarnation counter entries _) =
    let
        field name =
            D.decodeValue (D.field name D.string) value |> Result.withDefault ""

        requestId =
            field "requestId"
    in
    case Dict.get requestId entries of
        Nothing ->
            Model incarnation counter entries []

        Just entry ->
            if field "databaseId" /= entry.databaseId then
                Model incarnation counter entries []

            else if field "type" == "failure" then
                Model incarnation
                    counter
                    entries
                    [ { requestId = requestId, databaseId = entry.databaseId, instance = field "instance", authGeneration = D.decodeValue (D.field "authGeneration" D.int) value |> Result.withDefault 0, namespace = field "namespace", manifest = field "manifest", databaseEpoch = field "databaseEpoch", phase = field "phase", code = field "code", certainty = field "certainty", operationIndex = D.decodeValue (D.field "operationIndex" D.int) value |> Result.toMaybe } ]

            else if field "type" /= "lifecycle" || List.member entry.state [ "confirmed", "rejected", "acceptedUnreconciled" ] then
                Model incarnation counter entries []

            else
                let
                    nextState =
                        field "state"

                    results =
                        D.decodeValue (D.field "results" D.value) value |> Result.toMaybe

                    valid =
                        if List.member nextState [ "accepted", "confirmed", "acceptedUnreconciled" ] then
                            results |> Maybe.map (D.decodeValue entry.validate >> Result.toMaybe >> (/=) Nothing) |> Maybe.withDefault False

                        else
                            case nextState of
                                "queued" ->
                                    entry.state == "queued"

                                "locallyApplied" ->
                                    List.member entry.state [ "queued", "locallyApplied" ]

                                "sent" ->
                                    List.member entry.state [ "queued", "locallyApplied", "sent" ]

                                "rejected" ->
                                    entry.state /= "accepted"

                                "outcomeUnknown" ->
                                    entry.state /= "accepted"

                                _ ->
                                    False
                in
                if valid then
                    Model incarnation
                        counter
                        (Dict.insert requestId
                            { entry
                                | state = nextState
                                , results = results
                                , code = field "code"
                                , sequence = D.decodeValue (D.field "sequence" (D.map Just D.int)) value |> Result.withDefault entry.sequence
                                , commitRevision = D.decodeValue (D.field "commitRevision" (D.map Just D.int)) value |> Result.withDefault entry.commitRevision
                                , quarantined = D.decodeValue (D.field "quarantined" D.bool) value |> Result.withDefault False
                            }
                            entries
                        )
                        []

                else
                    Model incarnation counter entries []
