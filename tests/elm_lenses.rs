use pyre::ast;
use pyre::filesystem::GeneratedFile;
use pyre::generate::client::elm;
use pyre::parser;
use pyre::typecheck;
use std::collections::HashSet;
use std::path::Path;

fn path_ends_with(path: &Path, suffix: &str) -> bool {
    path.ends_with(Path::new(suffix))
}

#[test]
fn generated_elm_lenses_are_uniquely_named_for_nested_siblings() {
    let schema_source = r#"
record Rulebook {
    @public

    versions @link(RulebookVersion.rulebookId)

    id   Id.Uuid @id
    name String
}

record RulebookVersion {
    @public

    documents @link(RulebookVersionDocument.rulebookVersionId)
    rules     @link(RulebookVersionRules.rulebookVersionId)

    id         Id.Uuid @id
    rulebookId Rulebook.id
    versionTag String
}

record RulebookDocument {
    @public

    id          Id.Uuid @id
    contentHash String
    content     String
}

record RulebookRules {
    @public

    id          Id.Uuid @id
    contentHash String
    content     String
}

record RulebookVersionDocument {
    @public

    rulebookDocument @link(rulebookDocumentId, RulebookDocument.id)

    id                 Id.Uuid @id
    rulebookVersionId  RulebookVersion.id
    rulebookDocumentId RulebookDocument.id
    path               String
    orderIndex         Int
}

record RulebookVersionRules {
    @public

    rulebookRules @link(rulebookRulesId, RulebookRules.id)

    id               Id.Uuid @id
    rulebookVersionId RulebookVersion.id
    rulebookRulesId   RulebookRules.id
    path              String
    orderIndex        Int
}
"#;

    let query_source = r#"
query GetRulebookVersionBundle($rulebookName: String, $versionTag: String) {
    rulebook {
        @where { name == $rulebookName }

        id
        name
        versions {
            @where { versionTag == $versionTag }

            id
            versionTag
            documents {
                id
                path
                orderIndex
                rulebookDocument {
                    id
                    contentHash
                    content
                }
            }
            rules {
                id
                path
                orderIndex
                rulebookRules {
                    id
                    contentHash
                    content
                }
            }
        }
    }
}
"#;

    let mut schema = ast::Schema::default();
    parser::run("schema.pyre", schema_source, &mut schema).expect("schema parses");

    let database = ast::Database {
        schemas: vec![schema],
    };
    let context = typecheck::check_schema(&database).expect("schema typechecks");

    let query_list = parser::parse_query("query.pyre", query_source).expect("query parses");
    let query_info = typecheck::check_queries(&query_list, &context).expect("query typechecks");

    let mut files: Vec<GeneratedFile<String>> = Vec::new();
    elm::generate_queries(
        &context,
        &query_info,
        &query_list,
        Path::new("client/elm"),
        &mut files,
    );

    let generated = files
        .iter()
        .find(|f| path_ends_with(&f.path, "Query/GetRulebookVersionBundle.elm"))
        .expect("generated query elm file");

    let content = &generated.contents;

    assert!(
        content.contains("module Query.GetRulebookVersionBundle exposing (encode, queryShape, applyDelta, decodeQueryDelta, QueryDelta(..)"),
        "generated query module should expose queryShape. Generated:\n{}",
        content
    );

    assert!(content.contains("rulebookVersionsDocumentsLens :"));
    assert!(content.contains("rulebookVersionsRulesLens :"));
    assert!(content.contains("rulebookVersionsDocumentsRulebookDocumentLens :"));
    assert!(content.contains("rulebookVersionsRulesRulebookRulesLens :"));

    assert!(content.contains("Just (Db.Delta.listField rulebookVersionsDocumentsLens)"));
    assert!(content.contains("Just (Db.Delta.listField rulebookVersionsRulesLens)"));
    assert!(content
        .contains("Just (Db.Delta.maybeField rulebookVersionsDocumentsRulebookDocumentLens)"));
    assert!(content.contains("Just (Db.Delta.maybeField rulebookVersionsRulesRulebookRulesLens)"));
    assert!(
        content.contains(
            "queryShape : Encode.Value\nqueryShape =\n    Encode.object\n        [ (\"rulebook\", Encode.object\n            [ (\"id\", Encode.bool True)"
        ) || content.contains(
            "queryShape : Encode.Value\nqueryShape =\n    Encode.object\n        [ (\"rulebook\", Encode.object\n            [ (\"@where\", Encode.object\n                [ (\"name\", Encode.object\n                    [ (\"$var\", Encode.string \"rulebookName\")"
        ),
        "generated queryShape should be exposed and indentation should remain nested. Generated:\n{}",
        content
    );
    assert!(
        content
            .contains("(\"versions\", Encode.object\n                [ (\"@where\", Encode.object")
            && content.contains("(\"$var\", Encode.string \"versionTag\")"),
        "generated queryShape should include placeholder-aware @where clauses. Generated:\n{}",
        content
    );

    let mut lens_names = HashSet::new();
    let mut nested_names = HashSet::new();

    for line in content.lines() {
        if line.contains("Lens : Db.Delta.") {
            let name = line.split(':').next().unwrap_or("").trim();
            assert!(
                lens_names.insert(name.to_string()),
                "duplicate lens type signature generated: {}",
                line
            );
        }

        if line.contains("NestedFields : String -> Maybe") {
            let name = line.split(':').next().unwrap_or("").trim();
            assert!(
                nested_names.insert(name.to_string()),
                "duplicate nested-fields signature generated: {}",
                line
            );
        }
    }
}
