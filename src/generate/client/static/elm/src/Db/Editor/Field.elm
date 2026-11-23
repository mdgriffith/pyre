module Db.Editor.Field exposing (Error(..), Metadata, Shape(..), Value(..), View)

{-| The app owns widgets and raw input. These are the values accepted by the
headless editor once a widget has produced a typed value.
-}


type Value input
    = Unset
    | Null
    | Value input


type Error
    = Required
    | NotNullable
    | WrongInput


type Shape
    = Scalar String
    | Sequence Shape
    | Dictionary Shape
    | Optional Shape
    | Choices String (List ( String, List ( String, Shape ) ))
    | Named String


type alias Metadata =
    { key : String
    , required : Bool
    , nullable : Bool
    , shape : Shape
    }


type alias View input =
    { metadata : Metadata
    , original : Maybe (Value input)
    , current : Value input
    , error : Maybe Error
    }
