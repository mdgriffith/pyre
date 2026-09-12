import { localEdits } from "@pyre/server/local-edits";
import { Main, Records, Commands, batch, type ProjectId } from "../../../../target/local-edits-fixture/typescript/edits";
import { manifest } from "../../../../target/local-edits-fixture/typescript/server";
import { openDatabase } from "./database";

const { database, close } = await openDatabase();
const { Project, Task, Audit } = Records;
try {
  // This process owns this new database and explicitly chooses its real seed session.
  const edits = localEdits.bind({ database, databaseId: "seed-example", namespace: Main, manifest, session: { userId: 7 } });
  // A fresh UUID can be allocated before the atomic batch to link its dependent rows.
  const projectId = crypto.randomUUID() as ProjectId;
  const outcome = await edits.submit(batch([
    Project.create({ id: projectId, name: "Seed project", owner: 7 }),
    Task.create({ id: crypto.randomUUID(), project: projectId, title: "First task", owner: 7 }),
    Audit.create({ message: "Seeded project and task" }),
    Commands.namedAudit({ message: "Named command in the same transaction" }),
  ]));
  if (outcome.kind !== "confirmed") throw new Error(JSON.stringify(outcome));
  console.log(JSON.stringify({ project: outcome.result[0].id, task: outcome.result[1].id,
    audit: outcome.result[2].id, named: outcome.result[3], revision: outcome.commitRevision }, null, 2));
} finally { close(); }
