port module AcquireChecks exposing (main)

import Db.Id
import Db.Id.Acquire as Acquire
import Db.Id.Broker as Broker
import Json.Decode as Decode
import Json.Encode as Encode
import Platform


port output : Encode.Value -> Cmd msg


main : Program () () Never
main =
    Platform.worker
        { init = \() -> ( (), output (Encode.object (List.map (Tuple.mapSecond Encode.bool) checks)) )
        , update = \_ model -> ( model, Cmd.none )
        , subscriptions = \_ -> Sub.none
        }


uuid1 : String
uuid1 =
    "01900000-0000-7000-8000-00000000000a"


uuid2 : String
uuid2 =
    "01900000-0000-7000-8000-00000000000b"


one : Acquire.Request (Db.Id.Uuid guard)
one =
    Acquire.start identity |> Acquire.id |> Acquire.request


two : Acquire.Request String
two =
    Acquire.start Tuple.pair
        |> Acquire.id
        |> Acquire.id
        |> Acquire.map (\( Db.Id.Uuid lhs, Db.Id.Uuid rhs ) -> lhs ++ rhs)
        |> Acquire.request
        |> Acquire.mapRequest ((++) "mapped:")


requestKey : List (Broker.Action msg) -> String
requestKey actions =
    case actions of
        [ Broker.Send wire, Broker.Wait key milliseconds ] ->
            if Decode.decodeValue Acquire.responseId wire == Ok key && milliseconds == 5000 then
                key

            else
                ""

        _ ->
            ""


success : String -> List String -> Encode.Value
success key ids =
    Encode.object [ ( "type", Encode.string "ids-acquired" ), ( "requestId", Encode.string key ), ( "ids", Encode.list Encode.string ids ) ]


invalid : List (Broker.Action (Result Acquire.Error String)) -> Bool
invalid actions =
    case actions of
        [ Broker.Deliver (Err (Acquire.InvalidResponse _)) ] ->
            True

        _ ->
            False


checks : List ( String, Bool )
checks =
    let
        ( first, firstActions ) =
            Broker.start two identity Broker.init

        ( both, secondActions ) =
            Broker.start one (Result.map (\(Db.Id.Uuid value) -> value)) first

        key1 =
            requestKey firstActions

        key2 =
            requestKey secondActions

        wire1 =
            success key1 [ uuid1, uuid2 ]

        ( pending, reply2 ) =
            Broker.receive (success key2 [ uuid2 ]) both

        ( finished, reply1 ) =
            Broker.receive wire1 pending

        ( afterTimeout, timeout ) =
            Broker.timeout key1 first

        ( replaced, replacement ) =
            Broker.replaceBridge first

        ( newer, newActions ) =
            Broker.start two identity replaced

        newKey =
            requestKey newActions

        ( afterStale, stale ) =
            Broker.receive wire1 newer

        ( afterMalformed, malformed ) =
            Broker.receive (success "invalid\n" [ uuid1, uuid2 ]) first

        ( failed, failure ) =
            Broker.receive (success key1 [ uuid1, uuid1 ]) first

        many =
            List.foldl (\_ plan -> Acquire.map (\values next -> next :: values) plan |> Acquire.id) (Acquire.start []) (List.range 1 (Acquire.maxIds + 1))
    in
    [ ( "out of order and mapped in plan order", key1 /= key2 && reply2 == [ Broker.Deliver (Ok uuid2) ] && reply1 == [ Broker.Deliver (Ok ("mapped:" ++ uuid1 ++ uuid2)) ] )
    , ( "duplicate response and retired timer", Tuple.second (Broker.receive wire1 finished) == [ Broker.Report (Broker.UnknownResponseId key1) ] && List.isEmpty (Tuple.second (Broker.timeout key1 finished)) )
    , ( "timeout", timeout == [ Broker.Deliver (Err Acquire.TimedOut) ] && List.isEmpty (Tuple.second (Broker.timeout key1 afterTimeout)) && Tuple.second (Broker.receive wire1 afterTimeout) == [ Broker.Report (Broker.UnknownResponseId key1) ] )
    , ( "replacement fences old response but permits new one", replacement == [ Broker.Deliver (Err Acquire.BridgeReplaced) ] && stale == [ Broker.Report (Broker.UnknownResponseId key1) ] && Tuple.second (Broker.receive (success newKey [ uuid1, uuid2 ]) afterStale) == reply1 )
    , ( "malformed correlation does not consume request"
      , (case malformed of
            [ Broker.Report (Broker.MalformedResponseId _) ] ->
                True

            _ ->
                False
        )
            && Tuple.second (Broker.receive wire1 afterMalformed)
            == reply1
      )
    , ( "duplicates fail and remove handler", invalid failure && Tuple.second (Broker.receive wire1 failed) == [ Broker.Report (Broker.UnknownResponseId key1) ] )
    , ( "wrong count / uppercase / version fail", List.all (\ids -> invalid (Tuple.second (Broker.receive (success key1 ids) first))) [ [ uuid1 ], [ uuid1, String.toUpper uuid2 ], [ uuid1, "01900000-0000-4000-8000-000000000000" ] ] )
    , ( "zero completes locally", Tuple.second (Broker.start (Acquire.request (Acquire.start "ready")) identity Broker.init) == [ Broker.Deliver (Ok "ready") ] )
    , ( "over limit fails locally"
      , case Acquire.request many of
            Acquire.Failed Acquire.TooManyIds ->
                True

            _ ->
                False
      )
    ]
