module Data.Schema exposing (ColumnInfo, IndexInfo, LinkInfo, LinkTarget, LinkType(..), PrimaryKey, PrimaryKeyKind(..), SchemaMetadata, TableMetadata, WireCodec(..), decodeIndexInfo, decodeLinkInfo, decodeLinkTarget, decodeLinkType, decodeSchemaMetadata, decodeTableMetadata)

import Dict exposing (Dict)
import Json.Decode as Decode
import Set exposing (Set)


type alias LinkInfo =
    { type_ : LinkType
    , from : String
    , to : LinkTarget
    }


type LinkType
    = ManyToOne
    | OneToMany
    | OneToOne


type alias LinkTarget =
    { table : String
    , column : String
    }


type alias IndexInfo =
    { field : String
    , unique : Bool
    , primary : Bool
    }


type alias ColumnInfo =
    { name : String
    , type_ : String
    , nullable : Bool
    , codec : Maybe WireCodec
    }


type WireCodec
    = StringCodec
    | SafeIntCodec
    | FloatCodec
    | BoolCodec
    | DateCodec
    | DateTimeCodec
    | JsonCodec
    | UuidCodec
    | ListCodec WireCodec
    | DictCodec WireCodec
    | NullableCodec WireCodec
    | EnumCodec (Set String)
    | TaggedUnionCodec (Dict String (Dict String WireCodec))
    | NamedCodec String WireCodec
    | ReferenceCodec String


type alias TableMetadata =
    { name : String
    , columns : Maybe (List ColumnInfo)
    , links : Dict String LinkInfo
    , indices : List IndexInfo
    , primaryKey : PrimaryKey
    }


type alias PrimaryKey =
    { name : String, kind : PrimaryKeyKind }


type PrimaryKeyKind
    = IntKey
    | UuidKey
    | UnsupportedKey


type alias SchemaMetadata =
    { tables : Dict String TableMetadata
    , queryFieldToTable : Dict String String
    }


decodeLinkType : Decode.Decoder LinkType
decodeLinkType =
    Decode.string
        |> Decode.andThen
            (\str ->
                case str of
                    "many-to-one" ->
                        Decode.succeed ManyToOne

                    "one-to-many" ->
                        Decode.succeed OneToMany

                    "one-to-one" ->
                        Decode.succeed OneToOne

                    _ ->
                        Decode.fail ("Unknown link type: " ++ str)
            )


decodeLinkInfo : Decode.Decoder LinkInfo
decodeLinkInfo =
    Decode.map3 LinkInfo
        (Decode.field "type" decodeLinkType)
        (Decode.field "from" Decode.string)
        (Decode.field "to" decodeLinkTarget)


decodeLinkTarget : Decode.Decoder LinkTarget
decodeLinkTarget =
    Decode.map2 LinkTarget
        (Decode.field "table" Decode.string)
        (Decode.field "column" Decode.string)


decodeIndexInfo : Decode.Decoder IndexInfo
decodeIndexInfo =
    Decode.map3 IndexInfo
        (Decode.field "field" Decode.string)
        (Decode.field "unique" Decode.bool)
        (Decode.field "primary" Decode.bool)


decodeTableMetadata : Decode.Decoder TableMetadata
decodeTableMetadata =
    Decode.map5 TableMetadata
        (Decode.field "name" Decode.string)
        (Decode.oneOf
            [ Decode.field "columns" (Decode.list decodeColumnInfo) |> Decode.map Just
            , Decode.succeed Nothing
            ]
        )
        (Decode.field "links" (Decode.dict decodeLinkInfo))
        (Decode.field "indices" (Decode.list decodeIndexInfo))
        (Decode.field "primaryKey"
            (Decode.map2 PrimaryKey
                (Decode.field "name" Decode.string
                    |> Decode.andThen
                        (\name ->
                            if String.isEmpty name then
                                Decode.fail "Empty primary key name"

                            else
                                Decode.succeed name
                        )
                )
                (Decode.field "kind" Decode.string
                    |> Decode.andThen
                        (\kind ->
                            case kind of
                                "int" ->
                                    Decode.succeed IntKey

                                "uuid" ->
                                    Decode.succeed UuidKey

                                "unsupported" ->
                                    Decode.succeed UnsupportedKey

                                _ ->
                                    Decode.fail "Unsupported primary key kind"
                        )
                )
            )
        )


decodeColumnInfo : Decode.Decoder ColumnInfo
decodeColumnInfo =
    Decode.map4 ColumnInfo
        (Decode.field "name" Decode.string)
        (Decode.field "type" Decode.string)
        (Decode.field "nullable" Decode.bool)
        (Decode.oneOf [ Decode.field "codec" decodeWireCodec |> Decode.map Just, Decode.succeed Nothing ])


decodeWireCodec : Decode.Decoder WireCodec
decodeWireCodec =
    Decode.field "kind" Decode.string
        |> Decode.andThen
            (\kind ->
                case kind of
                    "string" ->
                        Decode.succeed StringCodec

                    "safeInt" ->
                        Decode.succeed SafeIntCodec

                    "float" ->
                        Decode.succeed FloatCodec

                    "bool" ->
                        Decode.succeed BoolCodec

                    "date" ->
                        Decode.succeed DateCodec

                    "dateTime" ->
                        Decode.succeed DateTimeCodec

                    "json" ->
                        Decode.succeed JsonCodec

                    "uuid" ->
                        Decode.succeed UuidCodec

                    "list" ->
                        Decode.field "item" (Decode.lazy (\_ -> decodeWireCodec)) |> Decode.map ListCodec

                    "dict" ->
                        Decode.field "value" (Decode.lazy (\_ -> decodeWireCodec)) |> Decode.map DictCodec

                    "nullable" ->
                        Decode.field "value" (Decode.lazy (\_ -> decodeWireCodec)) |> Decode.map NullableCodec

                    "enum" ->
                        Decode.field "values" (Decode.list Decode.string) |> Decode.map (Set.fromList >> EnumCodec)

                    "taggedUnion" ->
                        Decode.field "variants" (Decode.dict (Decode.dict (Decode.lazy (\_ -> decodeWireCodec)))) |> Decode.map TaggedUnionCodec

                    "named" ->
                        Decode.map2 NamedCodec (Decode.field "name" Decode.string) (Decode.field "value" (Decode.lazy (\_ -> decodeWireCodec)))

                    "reference" ->
                        Decode.field "name" Decode.string |> Decode.map ReferenceCodec

                    _ ->
                        Decode.fail "Unknown wire codec"
            )


decodeSchemaMetadata : Decode.Decoder SchemaMetadata
decodeSchemaMetadata =
    Decode.map2 SchemaMetadata
        (Decode.field "tables" (Decode.dict decodeTableMetadata))
        (Decode.field "queryFieldToTable" (Decode.dict Decode.string))
