import { Main, Records, Commands, batch, type AuditId, type ProjectId } from "../../../../target/local-edits-fixture/typescript/edits";
import type { LocalEdits } from "../../local-edits";

declare const edits: LocalEdits<Main>;
const { Audit, Project, Task } = Records;
async function generatedInference() {
  const outcome = await edits.submit(batch([
    Project.create({ name: "typed", owner: 7 }),
    Audit.create({ message: "typed" }),
    Commands.namedAudit({ id: crypto.randomUUID() as AuditId, message: "typed" }),
  ]));
  if (outcome.kind === "confirmed") {
    const projectId: ProjectId = outcome.result[0].id;
    const auditId: AuditId = outcome.result[1].id;
    const date: Date = outcome.result[2].audit[0].updatedAt;
    edits.submit(Task.create({ project: projectId, title: "related", owner: 7 }));
    // @ts-expect-error Distinct UUID identity brands cannot be mixed.
    Task.create({ project: auditId, title: "wrong", owner: 7 });
    // @ts-expect-error The batch retains each generated result's ID brand.
    const wrong: AuditId = outcome.result[0].id;
    void [date, wrong];
  }
}
void generatedInference;
