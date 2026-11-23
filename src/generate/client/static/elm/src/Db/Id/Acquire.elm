module Db.Id.Acquire exposing
    ( Description
    , Error(..)
    , Pending
    , Plan
    , Request(..)
    , describe
    , encode
    , id
    , map
    , mapRequest
    , maxIds
    , request
    , responseDecoder
    , responseId
    , start
    )

{-| Describe a fixed number of new UUIDs. The plan and its application values
stay in Elm; only a request ID and count go to JavaScript.
-}

import Db.Id
import Json.Decode as Decode
import Json.Encode as Encode
import Set


type Plan a
    = Plan Int (List String -> Maybe ( a, List String ))


type Pending a
    = Pending Int (List String -> Maybe ( a, List String ))


type Request a
    = NeedsIds (Pending a)
    | Ready a
    | Failed Error


type Error
    = TooManyIds
    | InvalidRequestId
    | InvalidResponse String
    | GenerationFailed String
    | TimedOut
    | BridgeReplaced
    | RequestIdsExhausted


type alias Description =
    { requestId : String, count : Int }


{-| Maximum number of IDs in one local round trip. Keep in sync with the JS handler.
-}
maxIds : Int
maxIds =
    64


start : a -> Plan a
start value =
    Plan 0 (\ids -> Just ( value, ids ))


{-| Supply the next typed UUID argument, in pipeline order.
-}
id : Plan (Db.Id.Uuid guard -> a) -> Plan a
id (Plan count apply) =
    Plan (count + 1)
        (\ids ->
            case apply ids of
                Just ( use, next :: rest ) ->
                    Just ( use (Db.Id.uuid next), rest )

                _ ->
                    Nothing
        )


map : (a -> b) -> Plan a -> Plan b
map fn (Plan count apply) =
    Plan count (apply >> Maybe.map (Tuple.mapFirst fn))


request : Plan a -> Request a
request (Plan count apply) =
    if count > maxIds then
        Failed TooManyIds

    else if count == 0 then
        case apply [] of
            Just ( value, [] ) ->
                Ready value

            _ ->
                Failed (InvalidResponse "Invalid zero-ID plan")

    else
        NeedsIds (Pending count apply)


mapRequest : (a -> b) -> Request a -> Request b
mapRequest fn needed =
    case needed of
        NeedsIds (Pending count apply) ->
            NeedsIds (Pending count (apply >> Maybe.map (Tuple.mapFirst fn)))

        Ready value ->
            Ready (fn value)

        Failed err ->
            Failed err


validRequestId : String -> Bool
validRequestId value =
    String.length value
        > 0
        && String.length value
        <= 128
        && String.all
            (\c ->
                (c >= '0' && c <= '9')
                    || (c >= 'a' && c <= 'z')
                    || (c >= 'A' && c <= 'Z')
                    || List.member c [ '.', '_', ':', '-' ]
            )
            value


describe : String -> Pending a -> Result Error Description
describe requestId (Pending count _) =
    if validRequestId requestId then
        Ok { requestId = requestId, count = count }

    else
        Err InvalidRequestId


encode : Description -> Encode.Value
encode description =
    Encode.object
        [ ( "type", Encode.string "acquire-ids" )
        , ( "requestId", Encode.string description.requestId )
        , ( "count", Encode.int description.count )
        ]


{-| Read only the correlation identity. A malformed body can still be routed
to its pending request, which should then fail exactly once.
-}
responseId : Decode.Decoder String
responseId =
    Decode.field "requestId" Decode.string
        |> Decode.andThen
            (\value ->
                if validRequestId value then
                    Decode.succeed value

                else
                    Decode.fail "Invalid acquisition request ID"
            )


responseDecoder : String -> Pending a -> Decode.Decoder (Result Error a)
responseDecoder expectedId (Pending count apply) =
    responseId
        |> Decode.andThen
            (\actualId ->
                if actualId /= expectedId then
                    Decode.fail "Unexpected acquisition request ID"

                else
                    Decode.field "type" Decode.string
                        |> Decode.andThen
                            (\kind ->
                                case kind of
                                    "ids-acquired" ->
                                        Decode.field "ids" (Decode.list Decode.string)
                                            |> Decode.andThen
                                                (\ids ->
                                                    if List.length ids /= count || not (List.all isUuidV7 ids) || Set.size (Set.fromList ids) /= count then
                                                        Decode.fail "Expected the requested number of distinct canonical lowercase UUIDv7s"

                                                    else
                                                        case apply ids of
                                                            Just ( value, [] ) ->
                                                                Decode.succeed (Ok value)

                                                            _ ->
                                                                Decode.fail "Could not apply acquisition plan"
                                                )

                                    "ids-failed" ->
                                        Decode.field "error" Decode.string
                                            |> Decode.map (GenerationFailed >> Err)

                                    _ ->
                                        Decode.fail "Unknown acquisition response type"
                            )
            )


isUuidV7 : String -> Bool
isUuidV7 value =
    case String.split "-" value of
        [ timeHigh, timeLow, version, variant, rest ] ->
            List.map String.length [ timeHigh, timeLow, version, variant, rest ]
                == [ 8, 4, 4, 4, 12 ]
                && String.startsWith "7" version
                && String.any (\c -> List.member c [ '8', '9', 'a', 'b' ]) (String.left 1 variant)
                && List.all (String.all (\c -> (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f'))) [ timeHigh, timeLow, version, variant, rest ]

        _ ->
            False
