pub mod fuzz;
pub mod snapshot;

/// Schema with session + @allow permissions, mirroring the shapes that
/// triggered the aliased-CTE permission predicate regression.
pub fn permissions_schema() -> String {
    r#"
session {
    userId Int
    role String
}

record Post {
    id Int @id
    title String
    content String
    authorId Int
    published Bool
    comments @link(Comment.postId)
    @allow(*) { authorId == Session.userId }
}

record Comment {
    id Int @id
    content String
    postId Int
    authorId Int
    post @link(postId, Post.id)
    @allow(*) { authorId == Session.userId }
}

record Article {
    id Int @id
    title String
    content String
    authorId Int
    status String
    @allow(query) { authorId == Session.userId || status == "published" }
    @allow(insert, update, delete) { authorId == Session.userId }
}

record Document {
    id Int @id
    title String
    content String
    ownerId Int
    visibility String
    @allow(query) { ownerId == Session.userId || visibility == "public" }
    @allow(insert, update) { ownerId == Session.userId }
    @allow(delete) { ownerId == Session.userId && Session.role == "admin" }
}
"#
    .to_string()
}

/// Schema exercising Json<T> columns (typed JSON documents).
pub fn json_schema() -> String {
    r#"
record Event {
    id Int @id
    name String
    payload Json<Lifecycle>
    tags Json<List<String>>
    counts Json<Dict<Int>>
    @public
}

type Lifecycle
   = Draft
   | Published {
        publishedAt String
     }
   | Archived
"#
    .to_string()
}
