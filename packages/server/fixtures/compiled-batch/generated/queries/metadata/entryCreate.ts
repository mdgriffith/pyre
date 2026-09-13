import { z } from 'zod';
import { CoercedBool, CoercedDate } from '../../decode';
import * as Decode from '../../decode';

export const RawInputValidator = z.object({
  id: z.string().length(36).regex(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i),
  release: z.string(),
  enabled: z.boolean(),
  count: z.number().int(),
  role: Decode.RoleWrite,
  details: Decode.DetailsJsonInput
});
const InputValidator = z.object({
  id: z.string().length(36).regex(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i),
  release: z.string(),
  enabled: z.boolean(),
  count: z.number().int(),
  role: Decode.RoleWrite,
  details: Decode.DetailsJsonInput
});
export type Input = z.infer<typeof RawInputValidator>;

// The Return Data!
const Entry = z.object({
  id: z.string().length(36).regex(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i),
  release: z.string(),
  enabled: CoercedBool,
  count: z.number(),
  role: Decode.Role,
  details: Decode.Details,
  updatedAt: CoercedDate
});


const ReturnData = z.object({
  entry: Entry.array()
});

export type Result = z.infer<typeof ReturnData>;

export const meta = {
  id: "d7fba8953361803f515b8d6eb046b632ed3151ef777dcf1b33bac104b8cc5de5",
  generatedEdit: { kind: "create" as const, writeStatementIndices: [0], writableInputs: ["id","release","enabled","count","role","details"] },
  primary_db: "_default",
  attached_dbs: [],
  operation: "insert" as const,
  session_args: [ ],
  json_session_args: [ ],
  json_session_validators: {
  },
  optional_input_args: [],
  json_input_args: ["details"],
  InputValidator,
  SessionValidator: Decode.SessionValidator,
  ReturnData,
};
