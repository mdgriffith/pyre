use pyre::server::{
    manifest::{Manifest, PyreSession},
    query::{self, BatchBinding},
    sync::SyncServer,
};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::Path,
    process::Command,
};

// A test-only JSON-lines transport for the route-independent Rust library. No
// SQL, authority or mocked response is supplied by the client under test.
pub fn run(root: &Path, generated: &Path) {
    let wasm = Command::new("npx")
        .args([
            "--yes",
            "--package",
            "wasm-pack",
            "wasm-pack",
            "build",
            "wasm",
            "--target",
            "web",
            "--out-dir",
            "../target/conformance-wasm",
            "--out-name",
            "pyre_wasm",
            "--dev",
        ])
        .env(
            "CARGO_TARGET_DIR",
            root.join("target/wasm-conformance-build"),
        )
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        wasm.status.success(),
        "{}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    let directory = generated.join("conformance");
    let generated = directory.as_path();
    let mut schema = pyre::ast::Schema::default();
    pyre::parser::run(
        "schema.pyre",
        include_str!("../fixtures/elm-local-edits/schema.pyre"),
        &mut schema,
    )
    .unwrap();
    let mut archive = pyre::ast::Schema {
        namespace: "Archive".into(),
        ..Default::default()
    };
    pyre::parser::run(
        "archive.pyre",
        include_str!("../fixtures/elm-local-edits/archive.pyre"),
        &mut archive,
    )
    .unwrap();
    let mut database = pyre::ast::Database {
        schemas: vec![schema, archive],
    };
    pyre::ast::resolve_id_brands(&mut database);
    let context = pyre::typecheck::check_schema(&database).unwrap();
    let mut queries = pyre::parser::parse_query("commands.pyre", "insert NamedAudit($message: String) { audit { message = $message id updatedAt } }\nquery ReadIssues { issue { id title } }\nquery ReadArchive { archiveEntry { id title } }").unwrap();
    pyre::generated_queries::append_generated_crud_queries(&mut queries, &context);
    let info = pyre::typecheck::check_queries(&queries, &context).unwrap();
    let mut files = Vec::new();
    pyre::generate::client::elm::generate(Path::new("src"), &database, &mut files);
    pyre::generate::client::elm::generate_queries(
        &context,
        &info,
        &queries,
        Path::new("src"),
        &mut files,
    );
    pyre::generate::typescript::core::generate_schema(
        &context,
        &database,
        Path::new("typescript/core"),
        &mut files,
    );
    pyre::generate::typescript::core::generate_queries(
        &context,
        &info,
        &queries,
        Path::new("typescript/core"),
        &mut files,
    );
    pyre::generate::typescript::targets::server::generate_schema(
        &context,
        &database,
        Path::new("typescript"),
        &mut files,
    );
    pyre::generate::typescript::targets::server::generate_queries(
        &context,
        &info,
        &queries,
        Path::new("typescript"),
        &mut files,
    );
    pyre::generate::manifest::generate_queries(&context, &queries, &info, &mut files);
    for file in files {
        let path = generated.join(file.path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, file.contents).unwrap();
    }
    let typecheck = Command::new(root.join("node_modules/.bin/tsc"))
        .args([
            "--noEmit",
            "--skipLibCheck",
            "--strict",
            "--module",
            "preserve",
            "--moduleResolution",
            "bundler",
            "--target",
            "es2022",
            "typescript/server.ts",
            "typescript/edits/Main.ts",
            "typescript/edits/Archive.ts",
        ])
        .current_dir(generated)
        .output()
        .unwrap();
    assert!(
        typecheck.status.success(),
        "{}{}",
        String::from_utf8_lossy(&typecheck.stdout),
        String::from_utf8_lossy(&typecheck.stderr)
    );
    fs::write(
        generated.join("elm.json"),
        include_str!("../fixtures/elm-local-edits/elm.json"),
    )
    .unwrap();
    fs::write(
        generated.join("src/Test.elm"),
        include_str!("../fixtures/elm-local-edits/Conformance.elm"),
    )
    .unwrap();
    fs::copy(
        generated.parent().unwrap().join("worker.js"),
        generated.join("worker.js"),
    )
    .unwrap();
    let compile = Command::new("npx")
        .args([
            "--yes",
            "--package",
            "elm@0.19.1-6",
            "elm",
            "make",
            "src/Test.elm",
            "--output=test.js",
        ])
        .current_dir(generated)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    fs::write(
        generated.join("src/ArchiveConformance.elm"),
        include_str!("../fixtures/elm-local-edits/ArchiveConformance.elm"),
    )
    .unwrap();
    let compile = Command::new("npx")
        .args([
            "--yes",
            "--package",
            "elm@0.19.1-6",
            "elm",
            "make",
            "src/ArchiveConformance.elm",
            "--output=archive.js",
        ])
        .current_dir(generated)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let address = listener.local_addr().unwrap();
    let manifest: Manifest =
        serde_json::from_str(&fs::read_to_string(generated.join("manifest.json")).unwrap())
            .unwrap();
    let db_path = generated.join("rust.db");
    let archive_path = generated.join("rust-archive.db");
    let server = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let db = libsql::Builder::new_local(db_path).build().await.unwrap();
            let conn = db.connect().unwrap();
            let archive_db = libsql::Builder::new_local(archive_path).build().await.unwrap();
            let archive_conn = archive_db.connect().unwrap();
            let fingerprint = manifest.fingerprint();
            let session = PyreSession::new(json!({}), &manifest.session_schema).unwrap();
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let message: Value = serde_json::from_str(&line).unwrap();
                if message["kind"] == "stop" { break; }
                let archive = message["database"] == "archive";
                let conn = if archive { &archive_conn } else { &conn };
                let binding = BatchBinding { database_id: if archive { "archive" } else { "one" }, namespace: if archive { "Archive" } else { "_default" }, manifest: &fingerprint, instance: "conformance", auth_generation: 1 };
                let response = match message["kind"].as_str().unwrap() {
                    "batch" => {
                        let request = serde_json::from_value(message["request"].clone()).unwrap();
                        match query::run_batch(&conn, &manifest, &binding, &request, &session).await {
                            Ok(result) => json!({"kind":"success", "response":result.response}),
                            Err(error) => json!({"kind":"error", "error":{"errorType":error.code(), "index":error.operation_index()}}),
                        }
                    }
                    "replacement" => {
                        let request = serde_json::from_value(message["request"].clone()).unwrap();
                        let loaded = pyre::server::schema::load_schema_from_database(conn).await.unwrap();
                        let replacement_context = if archive { loaded.context().unwrap() } else { &context };
                        match SyncServer::new(replacement_context).replacement(conn, &manifest, &binding, &request, &session).await {
                            Ok(response) => json!({"kind":"success", "response":response}),
                            Err(_) => json!({"kind":"error", "error":{"errorType":"InvalidRequest"}}),
                        }
                    }
                    _ => panic!("unknown conformance message"),
                };
                writeln!(stream, "{}", response).unwrap();
            }
        });
    });
    let output = Command::new("npx")
        .args([
            "--yes",
            "--package",
            "bun@latest",
            "bun",
            "test",
            "--preload",
            "./tests/fixtures/elm-local-edits/wasm.preload.ts",
            "tests/fixtures/elm-local-edits/conformance.test.ts",
            "tests/fixtures/elm-local-edits/native-browser.test.ts",
        ])
        .env("PYRE_ELM_EDIT_FIXTURE", generated)
        .env("PYRE_RUST_EDIT_ADDRESS", address.to_string())
        .env("BUN_RUNTIME_TRANSPILER_CACHE_PATH", "0")
        .env(
            "PYRE_CONFORMANCE_WASM",
            root.join("target/conformance-wasm/pyre_wasm_bg.wasm"),
        )
        .current_dir(root)
        .output()
        .unwrap();
    let mut stop = std::net::TcpStream::connect(address).unwrap();
    writeln!(stop, "{}", json!({"kind":"stop"})).unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprint!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
