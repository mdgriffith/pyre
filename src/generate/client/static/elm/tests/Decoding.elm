module Decoding exposing (..)

import Db.Read
import Expect exposing (Expectation)
import Fuzz exposing (Fuzzer, int, list, string)
import Json.Encode as Encode
import Test exposing (Test)
import Time


type alias User =
    { id : Int
    , email : String
    , rating : Float
    }


encodeUser : User -> Encode.Value
encodeUser user =
    Encode.object
        [ ( "id", Encode.int user.id )
        , ( "email", Encode.string user.email )
        , ( "rating", Encode.float user.rating )
        ]


type alias UserWithPosts =
    { id : Int
    , email : String
    , posts : List Post
    , rating : Float
    , accounts : List Account
    }


type alias Post =
    { id : Int
    , title : String
    , content : String
    }


type alias Account =
    { id : Int
    , name : String
    }


type alias Event =
    { id : Int
    , startedAt : Time.Posix
    }


encodeUserWithPostsAsRectangle : UserWithPosts -> List Encode.Value
encodeUserWithPostsAsRectangle user =
    user.posts
        |> List.concatMap
            (\post ->
                List.map
                    (\account ->
                        Encode.object
                            [ ( "id", Encode.int user.id )
                            , ( "email", Encode.string user.email )
                            , ( "rating", Encode.float user.rating )
                            , ( "postId", Encode.int post.id )
                            , ( "postTitle", Encode.string post.title )
                            , ( "postContent", Encode.string post.content )
                            , ( "accountId", Encode.int account.id )
                            , ( "accountName", Encode.string account.name )
                            ]
                    )
                    user.accounts
            )


suite : Test.Test
suite =
    Test.describe "Rectangular Results"
        [ Test.test "Basic Decoding (no nesting)" <|
            \_ ->
                let
                    users =
                        [ User 1 "email" 5.0
                        , User 2 "email2" 7.0
                        , User 3 "email3" 10.0
                        ]

                    data =
                        Encode.list encodeUser users

                    userDecoder =
                        Db.Read.query User [ Db.Read.id "id" ]
                            |> Db.Read.field "id" Db.Read.int
                            |> Db.Read.field "email" Db.Read.string
                            |> Db.Read.field "rating" Db.Read.float
                in
                Expect.equal
                    (Db.Read.decodeValue userDecoder data)
                    (Ok users)
        , Test.test "Nested Decoding" <|
            \_ ->
                let
                    users =
                        [ UserWithPosts 1 "email" [ Post 1 "Post 1" "Content1" ] 5.0 [ Account 1 "Account 1" ]
                        , UserWithPosts 2 "email2" [ Post 2 "Post 2" "Content1" ] 7.0 [ Account 4 "Account 4" ]
                        , UserWithPosts 3 "email3" [ Post 3 "Post 3" "Content1", Post 4 "Post 4" "Content1" ] 10.0 [ Account 6 "Account 6" ]
                        ]

                    data =
                        Encode.list identity
                            (List.concatMap encodeUserWithPostsAsRectangle users)

                    userDecoder =
                        Db.Read.query UserWithPosts [ Db.Read.id "id" ]
                            |> Db.Read.field "id" Db.Read.int
                            |> Db.Read.field "email" Db.Read.string
                            |> Db.Read.nested
                                (Db.Read.id "id")
                                (Db.Read.id "postId")
                                (Db.Read.query Post [ Db.Read.id "postId" ]
                                    |> Db.Read.field "postId" Db.Read.int
                                    |> Db.Read.field "postTitle" Db.Read.string
                                    |> Db.Read.field "postContent" Db.Read.string
                                )
                            |> Db.Read.field "rating" Db.Read.float
                            |> Db.Read.nested
                                (Db.Read.id "id")
                                (Db.Read.id "accountId")
                                (Db.Read.query Account [ Db.Read.id "accountId" ]
                                    |> Db.Read.field "accountId" Db.Read.int
                                    |> Db.Read.field "accountName" Db.Read.string
                                )
                in
                Expect.equal
                    (Db.Read.decodeValue userDecoder data)
                    (Ok users)
        , Test.test "DateTime decodes unix seconds" <|
            \_ ->
                let
                    data =
                        Encode.list identity
                            [ Encode.object
                                [ ( "id", Encode.int 1 )
                                , ( "startedAt", Encode.int 1783787812 )
                                ]
                            ]

                    eventDecoder =
                        Db.Read.query Event [ Db.Read.id "id" ]
                            |> Db.Read.field "id" Db.Read.int
                            |> Db.Read.field "startedAt" Db.Read.dateTime
                in
                Expect.equal
                    (Db.Read.decodeValue eventDecoder data)
                    (Ok
                        [ Event 1 (Time.millisToPosix 1783787812000)
                        ]
                    )
        ]
