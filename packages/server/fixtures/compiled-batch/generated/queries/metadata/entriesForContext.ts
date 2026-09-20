import { z } from 'zod';
import * as $Ids from '../../ids';
import type { GeneratedQueryShape } from '@pyre/core';
import * as Decode from '../../decode';

export const RawInputValidator = z.object({
});
const InputValidator = z.object({
});
export type Input = z.infer<typeof RawInputValidator>;

const queryShape: GeneratedQueryShape = { "$error": "Local queries cannot reference Session; use explicit inputs or execute on the server." };

// The Return Data!
const Entry = z.object({
  id: $Ids.EntryId
});


const ReturnData = z.object({
  entry: Entry.array()
});

export type Result = z.infer<typeof ReturnData>;

export const meta = {
  id: "f4bdfe31e86b76b72c4e56c135ad7383af79e1aa9613b9301ac79f2a9f08a115",
  schemaContracts: {"_default":"c8621ce5b5d01bde643f5a677495ad77b36dce46298214721d6895ab1258c01d"},
  primary_db: "_default",
  attached_dbs: [],
  operation: "query" as const,
  session_args: [ "context"],
  json_session_args: [ "context"],
  json_session_validators: {
    "context": z.lazy(() => Decode.DetailsSessionJsonInput),
  },
  optional_input_args: [],
  json_input_args: [],
  InputValidator,
  SessionValidator: Decode.SessionValidator,
  ReturnData,
  queryShape,
  toQueryShape: (_input: Input) => queryShape,
};
