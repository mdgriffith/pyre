module Db.Internal.Editor exposing
    ( Entry
    , Form
    , andMap
    , entry
    , errors
    , fields
    , formError
    , init
    , nullable
    , jsonEquality
    , set
    , validate
    , value
    )

import Db.Edit
import Db.Editor.Field as Field
import Dict exposing (Dict)
import Json.Decode as Decode
import Json.Encode as Encode


type Form input result
    = Form (List (Entry input)) (Dict String Field.Error) (Maybe String) ((String -> Field.Value input) -> Maybe result)


type alias Entry input =
    { metadata : Field.Metadata
    , original : Maybe (Field.Value input)
    , current : Field.Value input
    , accepts : input -> Bool
    , equal : input -> input -> Bool
    }


entry : Field.Metadata -> Maybe (Field.Value input) -> (input -> Bool) -> (input -> input -> Bool) -> Entry input
entry metadata original accepts equal =
    { metadata = metadata
    , original = original
    , current = Maybe.withDefault Field.Unset original
    , accepts = accepts
    , equal = equal
    }


init : ((String -> Field.Value input) -> Maybe result) -> List (Entry input) -> Form input result
init build entries =
    Form entries Dict.empty Nothing build


fields : Form input result -> List (Field.View input)
fields (Form entries feedback _ _) =
    List.map
        (\item ->
            { metadata = item.metadata
            , original = item.original
            , current = item.current
            , error = Dict.get item.metadata.key feedback
            }
        )
        entries


errors : Form input result -> List ( String, Field.Error )
errors (Form _ feedback _ _) =
    Dict.toList feedback


formError : Form input result -> Maybe String
formError (Form _ _ problem _) =
    problem


set : String -> Field.Value input -> Form input result -> Form input result
set key next (Form entries feedback _ build) =
    if List.any (\item -> item.metadata.key == key) entries then
        let
            change item =
                if item.metadata.key == key then
                    { item
                        | current =
                            case next of
                                Field.Unset ->
                                    Maybe.withDefault next item.original

                                _ ->
                                    next
                    }

                else
                    item
        in
        Form (List.map change entries) (Dict.remove key feedback) Nothing build

    else
        Form entries feedback (Just ("Unknown editor field: " ++ key)) build


validate : Form input result -> ( Maybe result, Form input result )
validate (Form entries _ problem build) =
    let
        fieldError item =
            case item.current of
                Field.Unset ->
                    if item.metadata.required then
                        Just Field.Required

                    else
                        Nothing

                Field.Null ->
                    if item.metadata.nullable then
                        Nothing

                    else
                        Just Field.NotNullable

                Field.Value input ->
                    if item.accepts input then
                        Nothing

                    else
                        Just Field.WrongInput

        feedback =
            List.filterMap (\item -> Maybe.map (Tuple.pair item.metadata.key) (fieldError item)) entries
                |> Dict.fromList

        changed item =
            if unchanged item then
                Field.Unset

            else
                item.current

        values =
            List.map (\item -> ( item.metadata.key, changed item )) entries |> Dict.fromList

        result =
            if Dict.isEmpty feedback && problem == Nothing then
                build (\key -> Dict.get key values |> Maybe.withDefault Field.Unset)

            else
                Nothing
    in
    ( result, Form entries feedback problem build )


unchanged : Entry input -> Bool
unchanged item =
    case ( item.original, item.current ) of
        ( Just (Field.Value original), Field.Value current ) ->
            item.equal original current

        ( Just Field.Null, Field.Null ) ->
            True

        _ ->
            False


{-| Elm equality on opaque JSON values uses JavaScript object representation.
Normalize them to Elm data so object keys are unordered and arrays and objects
cannot compare equal, even when empty.
-}
jsonEquality : (input -> Maybe Encode.Value) -> input -> input -> Bool
jsonEquality encode left right =
    let
        normalize input =
            encode input |> Maybe.andThen (Decode.decodeValue jsonTree >> Result.toMaybe)
    in
    case ( normalize left, normalize right ) of
        ( Just a, Just b ) ->
            a == b

        _ ->
            False


type JsonTree
    = JsonNull
    | JsonBool Bool
    | JsonNumber Float
    | JsonString String
    | JsonArray (List JsonTree)
    | JsonObject (Dict String JsonTree)


jsonTree : Decode.Decoder JsonTree
jsonTree =
    Decode.oneOf
        [ Decode.null JsonNull
        , Decode.map JsonBool Decode.bool
        , Decode.map JsonNumber Decode.float
        , Decode.map JsonString Decode.string
        , Decode.map JsonArray (Decode.list (Decode.lazy (\_ -> jsonTree)))
        , Decode.map JsonObject (Decode.dict (Decode.lazy (\_ -> jsonTree)))
        ]


value : (input -> Maybe a) -> Field.Value input -> Maybe a
value extract field =
    case field of
        Field.Value input ->
            extract input

        _ ->
            Nothing


nullable : (input -> Maybe a) -> Field.Value input -> Db.Edit.Field a
nullable extract field =
    case field of
        Field.Null ->
            Db.Edit.Null

        _ ->
            value extract field |> Maybe.map Db.Edit.Present |> Maybe.withDefault Db.Edit.Absent


andMap : Maybe a -> Maybe (a -> b) -> Maybe b
andMap =
    Maybe.map2 (|>)
