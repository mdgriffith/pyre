import { Main, Audit, Project, Task, Commands, batch, type AuditId, type ProjectId } from "../../../../target/local-edits-fixture/typescript/edits";
import type { LocalEdits } from "../../local-edits";

declare const edits: LocalEdits<Main>;
async function generatedInference() {
  const outcome = await edits.submit(batch([
    Project.create({ id: crypto.randomUUID(), name: "typed", owner: 7 }),
    Audit.create({ message: "typed" }),
    Commands.namedAudit({ message: "typed" }),
  ]));
  if (outcome.kind === "confirmed") {
    const projectId: ProjectId = outcome.result[0].id;
    const auditId: AuditId = outcome.result[1].id;
    const date: Date = outcome.result[2].audit[0].updatedAt;
    edits.submit(Task.create({ id: crypto.randomUUID(), project: projectId, title: "related", owner: 7 }));
    // @ts-expect-error Integer identities cannot be used as UUID references.
    Task.create({ id: crypto.randomUUID(), project: auditId, title: "wrong", owner: 7 });
    // @ts-expect-error The batch retains each generated result's ID brand.
    const wrong: AuditId = outcome.result[0].id;
    void [date, wrong];
  }
}
void generatedInference;
