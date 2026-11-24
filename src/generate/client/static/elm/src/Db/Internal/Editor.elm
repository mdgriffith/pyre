module Db.Internal.Editor exposing (Form, andMap, dictEqual, entry, errors, fields, formError, init, jsonEqual, listEqual, maybeEqual, nullable, set, validate, value)

import Db
import Db.Editor.Field as Field
import Dict exposing (Dict)
import Json.Decode as Decode
import Json.Encode as Encode


type Form fieldId kind value result
    = Form
        { entries : List (Entry fieldId kind value)
        , kind : value -> kind
        , build : (fieldId -> Db.Nullable value) -> Maybe result
        , problem : Maybe String
        }


type alias Entry fieldId kind value =
    { view : Field.View fieldId kind value
    , equal : value -> value -> Bool
    }


entry : { id : fieldId, kind : kind, required : Bool, nullable : Bool, choices : List String } -> Db.Nullable value -> (value -> value -> Bool) -> Entry fieldId kind value
entry info original equal =
    { view =
        { id = info.id
        , kind = info.kind
        , required = info.required
        , nullable = info.nullable
        , choices = info.choices
        , original = original
        , current = original
        , error = Nothing
        }
    , equal = equal
    }


init : (value -> kind) -> ((fieldId -> Db.Nullable value) -> Maybe result) -> List (Entry fieldId kind value) -> Form fieldId kind value result
init kind build entries =
    Form { entries = entries, kind = kind, build = build, problem = Nothing }


fields : Form fieldId kind value result -> List (Field.View fieldId kind value)
fields (Form form) =
    List.map .view form.entries


errors : Form fieldId kind value result -> List ( fieldId, Field.Error )
errors form =
    fields form |> List.filterMap (\view -> Maybe.map (Tuple.pair view.id) view.error)


formError : Form fieldId kind value result -> Maybe String
formError (Form form) =
    form.problem


set : fieldId -> Db.Nullable value -> Form fieldId kind value result -> Form fieldId kind value result
set id next (Form form) =
    let
        change item =
            let
                view =
                    item.view
            in
            if view.id == id then
                { item | view = { view | current = restore view.original next, error = Nothing } }

            else
                item
    in
    if List.any (\item -> item.view.id == id) form.entries then
        Form { form | entries = List.map change form.entries, problem = Nothing }

    else
        Form { form | problem = Just "Field is unavailable on this form" }


restore : Db.Nullable value -> Db.Nullable value -> Db.Nullable value
restore original next =
    case next of
        Db.Absent ->
            original

        _ ->
            next


validate : Form fieldId kind value result -> ( Maybe result, Form fieldId kind value result )
validate (Form form) =
    let
        fieldError view =
            case view.current of
                Db.Absent ->
                    if view.required then
                        Just Field.Required

                    else
                        Nothing

                Db.Null ->
                    if view.nullable then
                        Nothing

                    else
                        Just Field.NotNullable

                Db.Present input ->
                    if form.kind input == view.kind then
                        Nothing

                    else
                        Just Field.WrongKind

        check item =
            let
                view =
                    item.view
            in
            { item | view = { view | error = fieldError view } }

        checked =
            Form { form | entries = List.map check form.entries }

        changed item =
            case ( item.view.original, item.view.current ) of
                ( Db.Present original, Db.Present current ) ->
                    if item.equal original current then
                        Db.Absent

                    else
                        item.view.current

                ( Db.Null, Db.Null ) ->
                    Db.Absent

                _ ->
                    item.view.current

        get id =
            form.entries
                |> List.filter (\item -> item.view.id == id)
                |> List.head
                |> Maybe.map changed
                |> Maybe.withDefault Db.Absent
    in
    ( if form.problem == Nothing && List.isEmpty (errors checked) then
        form.build get

      else
        Nothing
    , checked
    )


value : (input -> Maybe a) -> Db.Nullable input -> Maybe a
value extract field =
    case field of
        Db.Present input ->
            extract input

        _ ->
            Nothing


nullable : (input -> Maybe a) -> Db.Nullable input -> Db.Nullable a
nullable extract field =
    case field of
        Db.Null ->
            Db.Null

        _ ->
            value extract field |> Maybe.map Db.Present |> Maybe.withDefault Db.Absent


andMap : Maybe a -> Maybe (a -> b) -> Maybe b
andMap =
    Maybe.map2 (|>)


listEqual : (a -> a -> Bool) -> List a -> List a -> Bool
listEqual equal left right =
    case ( left, right ) of
        ( [], [] ) ->
            True

        ( a :: moreLeft, b :: moreRight ) ->
            if equal a b then
                listEqual equal moreLeft moreRight

            else
                False

        _ ->
            False


dictEqual : (a -> a -> Bool) -> Dict comparable a -> Dict comparable a -> Bool
dictEqual equal left right =
    listEqual (\( key, a ) ( other, b ) -> key == other && equal a b) (Dict.toList left) (Dict.toList right)


maybeEqual : (a -> a -> Bool) -> Maybe a -> Maybe a -> Bool
maybeEqual equal left right =
    case ( left, right ) of
        ( Nothing, Nothing ) ->
            True

        ( Just a, Just b ) ->
            equal a b

        _ ->
            False


jsonEqual : Encode.Value -> Encode.Value -> Bool
jsonEqual left right =
    case ( normalizeJson left, normalizeJson right ) of
        ( Ok a, Ok b ) ->
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


{-| Elm's object decoder calls the object's `hasOwnProperty` method. Prefix
every string token (including keys) with a space before decoding, so valid
JSON keys cannot shadow it. The transformation preserves equality, and the
tree distinguishes arrays from objects and compares objects without key order.
-}
normalizeJson : Encode.Value -> Result Decode.Error JsonTree
normalizeJson input =
    let
        step char ( inString, escaped, reversed ) =
            if escaped then
                ( inString, False, char :: reversed )

            else if char == '"' then
                if inString then
                    ( False, False, char :: reversed )

                else
                    ( True, False, ' ' :: char :: reversed )

            else
                ( inString, inString && char == '\\', char :: reversed )

        ( _, _, chars ) =
            String.foldl step ( False, False, [] ) (Encode.encode 0 input)
    in
    Decode.decodeString jsonTree (String.fromList (List.reverse chars))
