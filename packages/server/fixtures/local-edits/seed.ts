import { localEdits } from "@pyre/server/local-edits";
import { Main, Records, Commands, batch, type AuditId } from "../../../../target/local-edits-fixture/typescript/edits";
import { manifest } from "../../../../target/local-edits-fixture/typescript/server";
import { openDatabase } from "./database";

const { database, close } = await openDatabase();
const { Project, Task, Audit } = Records;
try {
  // This process owns this new database and explicitly chooses its real seed session.
  const edits = localEdits.bind({ database, databaseId: "seed-example", namespace: Main, manifest, session: { userId: 7 } });
  const project = await edits.submit(Project.create({ name: "Seed project", owner: 7 }));
  if (project.kind !== "confirmed") throw new Error(JSON.stringify(project));
  const namedId = crypto.randomUUID() as AuditId;
  const outcome = await edits.submit(batch([
    Task.create({ project: project.result.id, title: "First task", owner: 7 }),
    Audit.create({ message: "Seeded project and task" }),
    Commands.namedAudit({ id: namedId, message: "Named command in the same transaction" }),
  ]));
  if (outcome.kind !== "confirmed") throw new Error(JSON.stringify(outcome));
  console.log(JSON.stringify({ project: project.result.id, task: outcome.result[0].id,
    audit: outcome.result[1].id, named: outcome.result[2], revision: outcome.commitRevision }, null, 2));
} finally { close(); }
