import { namespace, scopedBatch, scopedEdit, type Created, type Edit, type Batch } from "@pyre/core/local-edits";
import type { LocalEdits, Outcome } from "./local-edits";

declare const mainBrand: unique symbol;
declare const otherBrand: unique symbol;
type Main = typeof mainBrand;
type Other = typeof otherBrand;
type Id = number & { readonly brand: "Audit" };
declare const edits: LocalEdits<Main>;
declare const single: Edit<Main, Created<Id>>;
declare const other: Edit<Other, Created<Id>>;
declare const otherBatch: Batch<Other, readonly [Created<Id>]>;
const main = namespace<Main>("Main", "manifest");
const named = scopedEdit(main, { id: "named", parseInput: () => ({}), decodeResult: () => ({ date: new Date() }) }, {});

// This function is checked, never executed.
async function inference() {
  const singleResult: Promise<Outcome<Created<Id>>> = edits.submit(single);
  const result = await edits.submit(scopedBatch(main)([single, named]));
  if (result.kind === "confirmed") {
    const tuple: readonly [Created<Id>, { date: Date }] = result.result;
    const id: Id = tuple[0].id;
    // @ts-expect-error Result tuple retains distinct operation types.
    const wrong: Date = tuple[0].id;
    void [id, wrong];
  }
  if (result.kind === "acceptedUnreconciled") {
    const revision: number = result.commitRevision;
    // @ts-expect-error A committed decoding failure has no valid typed result.
    const invalid = result.result;
    void [revision, invalid];
  }
  // @ts-expect-error Wrong namespace edit cannot be submitted.
  edits.submit(other);
  // @ts-expect-error Wrong namespace batch cannot be submitted.
  edits.submit(otherBatch);
  void singleResult;
}
void inference;
