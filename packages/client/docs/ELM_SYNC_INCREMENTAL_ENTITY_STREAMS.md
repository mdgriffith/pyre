# Elm Sync Incremental Entity Streams

This guide shows how to consume Pyre table entity streams from an Elm application.

Entity streams are a lower-level sync API than live queries. They emit table rows directly:

- first, an IndexedDB initial batch for the stream
- then, matching catchup/live server table deltas
- always as app-owned events, not as Pyre-owned Elm state

## Generated Elm Module

`pyre generate` writes:

```text
pyre/generated/client/elm/EntityStream.elm
```

The generated module exposes one constructor per table:

```elm
type EntitySubscription
    = Post (Maybe (List Int))
    | Comment (Maybe (List String))
```

The `Maybe (List id)` is a primary-ID filter:

- `Nothing` subscribes to all rows for that table.
- `Just ids` subscribes to rows whose `id` is in that list.

Returned rows decode to typed variants:

```elm
type EntityChange
    = PostRow PostEntity
    | PostRemoved PostId
    | CommentRow CommentEntity
    | CommentRemoved CommentId
    | EntityDecodeFailed String Decode.Value
```

Removal variants carry the schema primary-key type and no row payload.

## Elm Ports

Your Elm app needs the same outbound bridge port used by generated Pyre query code, plus the entity-stream receive port:

```elm
port pyreStoreOut : Encode.Value -> Cmd msg


port pyre_receiveEntityChanges : (Decode.Value -> msg) -> Sub msg
```

## Register A Stream

Use `EntityStream.register` to build the bridge message:

```elm
type Msg
    = RegisterVisiblePosts
    | EntityStreamReceived Decode.Value


registerVisiblePosts : Db.Database.DatabaseId Db.Database.Default -> Cmd Msg
registerVisiblePosts databaseId =
    EntityStream.register
        databaseId
        "visible-posts"
        [ EntityStream.Post (Just [ 1, 2, 3 ])
        , EntityStream.Comment Nothing
        ]
        |> pyreStoreOut
```

This sends TypeScript:

```json
{
  "type": "register-entity-stream",
  "databaseId": "main",
  "streamId": "visible-posts",
  "tables": [
    { "tableName": "posts", "where": { "id": { "$in": [1, 2, 3] } } },
    { "tableName": "comments" }
  ]
}
```

## Subscribe To Batches

Wire the receive port into subscriptions:

```elm
subscriptions : Model -> Sub Msg
subscriptions _ =
    pyre_receiveEntityChanges EntityStreamReceived
```

Decode incoming batches in `update`:

```elm
type alias Model =
    { posts : Dict Int EntityStream.PostEntity
    , comments : Dict String EntityStream.CommentEntity
    }


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        EntityStreamReceived value ->
            case EntityStream.decodeIncomingBatch value of
                Ok batch ->
                    ( applyEntityBatch batch model, Cmd.none )

                Err _ ->
                    ( model, Cmd.none )

        RegisterVisiblePosts ->
            ( model, Cmd.none )
```

Then apply changes into app-owned state:

```elm
applyEntityBatch : EntityStream.EntityChangeBatch -> Model -> Model
applyEntityBatch batch model =
    List.foldl applyEntityChange model batch.changes


applyEntityChange : EntityStream.EntityChange -> Model -> Model
applyEntityChange change model =
    case change of
        EntityStream.PostRow post ->
            { model | posts = Dict.insert post.id post model.posts }

        EntityStream.CommentRow comment ->
            { model | comments = Dict.insert comment.id comment model.comments }

        EntityStream.EntityDecodeFailed _ _ ->
            model
```

## Unregister A Stream

When the stream is no longer needed:

```elm
unregisterVisiblePosts : Cmd msg
unregisterVisiblePosts =
    EntityStream.unregister "visible-posts"
        |> pyreStoreOut
```

To change filters, unregister and register again with the same or a new `streamId`.

## TypeScript Bridge Setup

If you use `PyreClient.create` with `elm`, the default ports are already wired:

```ts
const client = await PyreClient.create({
  schema,
  server,
  cacheNamespace,
  elm: { app },
})
```

Defaults:

- Elm outbound: `pyreStoreOut`
- Entity stream inbound: `pyre_receiveEntityChanges`

If your Elm app uses different port names:

```ts
const client = await PyreClient.create({
  schema,
  server,
  cacheNamespace,
  elm: {
    app,
    receivePort: "myPyreOut",
    entityChangesPort: "myEntityChangesIn",
  },
})
```

## Batch Semantics

Every registered stream receives batches shaped like:

```elm
type alias EntityChangeBatch =
    { streamId : StreamId
    , databaseId : Maybe String
    , sequence : Int
    , source : EntityChangeBatchSource
    , changes : List EntityChange
    }
```

`source` is one of:

- `IndexedDbInitial`: the initial persisted snapshot. This is always sent first, even if `changes` is empty.
- `Catchup`: server catchup deltas observed after registration.
- `Live`: live server deltas observed after registration.
- `UnknownSource String`: forward-compatible fallback.

## V1 Limits

Entity streams intentionally do not provide:

- delete events
- insert versus update distinction
- previous row values
- field-level diffs
- membership-left events when a row stops matching a condition
- generated Elm filters beyond primary-ID lists

The Elm app owns storage, indexing, derived data, and filter-change behavior.
