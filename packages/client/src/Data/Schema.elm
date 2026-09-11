module Data.Schema exposing (IndexInfo, LinkInfo, LinkTarget, LinkType(..), PrimaryKey, PrimaryKeyKind(..), SchemaMetadata, TableMetadata, decodeIndexInfo, decodeLinkInfo, decodeLinkTarget, decodeLinkType, decodeSchemaMetadata, decodeTableMetadata)

import Dict exposing (Dict)
import Json.Decode as Decode


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


type alias TableMetadata =
    { name : String
    , links : Dict String LinkInfo
    , indices : List IndexInfo
    , primaryKey : PrimaryKey
    }


type alias PrimaryKey =
    { name : String, kind : PrimaryKeyKind }


type PrimaryKeyKind
    = IntKey
    | UuidKey


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
    Decode.map4 TableMetadata
        (Decode.field "name" Decode.string)
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

                                _ ->
                                    Decode.fail "Unsupported primary key kind"
                        )
                )
            )
        )


decodeSchemaMetadata : Decode.Decoder SchemaMetadata
decodeSchemaMetadata =
    Decode.map2 SchemaMetadata
        (Decode.field "tables" (Decode.dict decodeTableMetadata))
        (Decode.field "queryFieldToTable" (Decode.dict Decode.string))
