use std::io::{self, Write};

pub struct DocResource {
    pub topic: &'static str,
    pub uri: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub content: &'static str,
}

pub const DOC_RESOURCES: &[DocResource] = &[
    DocResource {
        topic: "getting-started",
        uri: "pyre://guides/getting-started",
        name: "Pyre Getting Started",
        description: "Project setup and first schema/query workflow.",
        content: include_str!("../../../docs/usage/getting-started.md"),
    },
    DocResource {
        topic: "schema",
        uri: "pyre://guides/schema",
        name: "Schema Guide",
        description: "How to write Pyre schemas: records, links, permissions, types, and sessions.",
        content: include_str!("../../../docs/usage/schema.md"),
    },
    DocResource {
        topic: "query",
        uri: "pyre://guides/query",
        name: "Query Guide",
        description:
            "Queries, named commands, generated CRUD, composed-operation semantics, and typed results.",
        content: include_str!("../../../docs/usage/query.md"),
    },
    DocResource {
        topic: "namespacing",
        uri: "pyre://guides/namespacing",
        name: "Pyre Namespacing",
        description: "Multi-schema namespace layout and reference rules.",
        content: include_str!("../../../docs/usage/namespacing.md"),
    },
    DocResource {
        topic: "sync",
        uri: "pyre://guides/sync",
        name: "Pyre Sync Setup",
        description: "Client/server sync: authentication, local reads, TypeScript composed writes, and recovery limits.",
        content: include_str!("../../../docs/usage/sync.md"),
    },
    DocResource {
        topic: "ephemeral-state",
        uri: "pyre://guides/ephemeral-state",
        name: "Ephemeral State",
        description: "Typed transient Connection and Shared state across schema, client, server, and deployment APIs.",
        content: include_str!("../../../docs/usage/ephemeral-state.md"),
    },
    DocResource {
        topic: "elm-sync",
        uri: "pyre://guides/elm-sync",
        name: "Elm + Sync Runtime Setup",
        description: "Elm integration: database-scoped queries, opaque edit builders, typed receipts, and ports.",
        content: include_str!("../../../docs/usage/elm-sync.md"),
    },
    DocResource {
        topic: "multi-database-upgrade",
        uri: "pyre://guides/multi-database-upgrade",
        name: "Multi-Database Upgrade Guide",
        description: "Extend a sync integration to multiple source databases within each schema family.",
        content: include_str!("../../../docs/usage/multi-database-upgrade.md"),
    },
    DocResource {
        topic: "server-contexts",
        uri: "pyre://guides/server-contexts",
        name: "Server Contexts Guide",
        description: "Optional server-only session-resolution caching API for TypeScript/Rust, TTL, and invalidation.",
        content: include_str!("../../../docs/usage/server-contexts.md"),
    },
    DocResource {
        topic: "seeding",
        uri: "pyre://guides/seeding",
        name: "Seeding And Server-Owned Writes",
        description: "Explicit-session composed execution versus the trusted import-oriented seed helper.",
        content: include_str!("../../../docs/usage/seeding.md"),
    },
    DocResource {
        topic: "rust-server",
        uri: "pyre://guides/rust-server",
        name: "Rust Server Integration",
        description: "Manifest execution, composed operations, authenticated origins, and incremental sync in Rust.",
        content: include_str!("../../../docs/usage/rust-server.md"),
    },
    DocResource {
        topic: "migrations",
        uri: "pyre://guides/migrations",
        name: "Pyre Migrations",
        description: "SQL generation and migration-relevant behavior.",
        content: include_str!("../../../docs/usage/migrations.md"),
    },
    DocResource {
        topic: "project-structure",
        uri: "pyre://guides/project-structure",
        name: "Pyre Project Structure",
        description: "Common filesystem layouts for Pyre projects.",
        content: include_str!("../../../docs/usage/project-structure.md"),
    },
    DocResource {
        topic: "serve",
        uri: "pyre://guides/serve",
        name: "Pyre Serve",
        description: "Built-in HTTP server usage and behavior.",
        content: include_str!("../../../docs/usage/pyre-serve.md"),
    },
    DocResource {
        topic: "troubleshooting",
        uri: "pyre://guides/troubleshooting",
        name: "Pyre Troubleshooting",
        description: "Common setup and local-development issues.",
        content: include_str!("../../../docs/usage/troubleshooting.md"),
    },
    DocResource {
        topic: "mcp",
        uri: "pyre://guides/mcp",
        name: "Pyre MCP",
        description: "Agent-oriented MCP workflow and CLI-to-MCP mapping.",
        content: include_str!("../../../docs/usage/mcp.md"),
    },
];

pub fn docs(topic: &Option<String>) -> io::Result<()> {
    let mut stdout = io::stdout().lock();

    match topic.as_deref() {
        Some(topic) => {
            let doc = find_doc(topic).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Unknown doc topic: {topic}"),
                )
            })?;
            stdout.write_all(doc.content.as_bytes())?;
            if !doc.content.ends_with('\n') {
                stdout.write_all(b"\n")?;
            }
        }
        None => {
            for doc in DOC_RESOURCES {
                writeln!(stdout, "{}\t{}", doc.topic, doc.description)?;
            }
        }
    }

    Ok(())
}

pub fn find_doc(topic: &str) -> Option<&'static DocResource> {
    DOC_RESOURCES.iter().find(|doc| doc.topic == topic)
}
