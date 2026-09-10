import { z } from 'zod';

function invalidDate(ctx: z.RefinementCtx, message: string): never {
  ctx.addIssue({ code: 'custom', message });
  return z.NEVER;
}

function parseRfc3339(value: string): Date | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})[Tt](?:[01]\d|2[0-3]):[0-5]\d:[0-5]\d(?:\.\d+)?(?:[Zz]|[+-](?:[01]\d|2[0-3]):[0-5]\d)$/.exec(value);
  if (!match) {
    return null;
  }

  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  const leapYear = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const daysInMonth = [31, leapYear ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  if (month < 1 || month > 12 || day < 1 || day > daysInMonth[month - 1]) {
    return null;
  }

  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? null : parsed;
}

export const CoercedDate = z.union([z.number(), z.string(), z.date()]).transform((val, ctx) => {
  if (val instanceof Date) {
    return Number.isNaN(val.getTime()) ? invalidDate(ctx, 'Invalid Date') : val;
  }

  if (typeof val === 'number') {
    if (!Number.isSafeInteger(val)) {
      return invalidDate(ctx, 'Expected whole Unix seconds');
    }
    const parsed = new Date(val * 1000);
    return Number.isNaN(parsed.getTime()) ? invalidDate(ctx, 'Unix seconds are outside the supported range') : parsed;
  }

  const trimmed = val.trim();
  if (/^[+-]?\d+$/.test(trimmed)) {
    const seconds = Number(trimmed);
    if (!Number.isSafeInteger(seconds)) {
      return invalidDate(ctx, 'Invalid Unix seconds');
    }
    const parsed = new Date(seconds * 1000);
    return Number.isNaN(parsed.getTime()) ? invalidDate(ctx, 'Unix seconds are outside the supported range') : parsed;
  }

  return parseRfc3339(trimmed) ?? invalidDate(ctx, 'Expected whole Unix seconds or an RFC 3339 timestamp');
});
export const CoercedBool = z.union([z.boolean(), z.literal(0), z.literal(1)]).transform((val) => typeof val === 'number' ? val === 1 : val);

// JSON values are decoded as unknown for type safety
export type Json = unknown;

export const Json: z.ZodType<Json> = z.unknown();

export function decodeOrThrow<T>(validator: z.ZodType<T>, data: unknown, label: string = 'data'): T {
  const decoded = validator.safeParse(data);
  if (!decoded.success) {
    const errorStr = JSON.stringify(decoded.error, null, 2);
    throw new Error(`Failed to decode ${label}: ${errorStr}`);
  }
  return decoded.data;
}

const RoleEnum = z.enum(["Member", "Admin"]);

export const Role = z.preprocess((value) => {
  if (typeof value === 'string') {
    return value;
  }

  if (value != null && typeof value === 'object' && !Array.isArray(value)) {
    const record = value as Record<string, unknown>;
    if (typeof record._type === 'string') {
      return record._type;
    }
  }

  return value;
}, RoleEnum);

export type Role = z.infer<typeof Role>;

export const RoleWrite = z.union([z.enum(["Member", "Admin"]), z.strictObject({ _type: z.enum(["Member", "Admin"]) })]).transform(value => typeof value === 'string' ? value : value._type);
export type RoleWrite = z.infer<typeof RoleWrite>;

export const RoleJsonInput = RoleWrite.transform(_type => ({ _type }));
export type RoleJsonInput = z.infer<typeof RoleJsonInput>;

export const RoleSessionInput = z.union([z.enum(["Member", "Admin"]), z.object({ _type: z.enum(["Member", "Admin"]) })]).transform(value => typeof value === 'string' ? value : value._type);
export type RoleSessionInput = z.infer<typeof RoleSessionInput>;

export const RoleSessionJsonInput = z.union([z.enum(["Member", "Admin"]), z.object({ _type: z.enum(["Member", "Admin"]) })]).transform(value => ({ _type: typeof value === 'string' ? value : value._type }));
export type RoleSessionJsonInput = z.infer<typeof RoleSessionJsonInput>;

export type Details =
  | { _type: "Note"; count?: number; enabled?: boolean }
  | { _type: "Empty" }
  | { _type: "Raw"; data?: unknown; values?: Array<number | null>; scalar?: string | null }
  | { _type: "Bundle"; when?: Date; role?: Role; children?: Array<Details>; byName?: Record<string, Details>; note?: string | null }
;

type DetailsInput =
  | { _type: "Note"; count?: number; enabled?: boolean | number }
  | { _type: "Empty" }
  | { _type: "Raw"; data?: unknown; values?: Array<number | null>; scalar?: string | null }
  | { _type: "Bundle"; when?: number | string | Date; role?: unknown; children?: Array<unknown>; byName?: Record<string, unknown>; note?: string | null }
;

const DetailsDiscriminated: z.ZodType<Details, DetailsInput> = z.discriminatedUnion("_type", [
  z.object({
    _type: z.literal("Note"),
    count: z.number().int().optional(),
    enabled: CoercedBool.optional(),
  }),
  z.object({
    _type: z.literal("Empty"),
  }),
  z.object({
    _type: z.literal("Raw"),
    data: Json.optional(),
    values: z.array(z.number().int().nullable()).optional(),
    scalar: z.string().nullable().optional(),
  }),
  z.object({
    _type: z.literal("Bundle"),
    when: CoercedDate.optional(),
    role: z.lazy(() => Role).optional(),
    children: z.array(z.lazy(() => Details)).optional(),
    byName: z.record(z.string(), z.lazy(() => Details)).optional(),
    note: z.string().nullish(),
  }),
]);

export const Details: z.ZodType<Details, unknown> = z.preprocess((value) => {
  if (value != null && typeof value === 'object' && !Array.isArray(value)) {
    const record = value as Record<string, unknown>;
    const normalized = { ...record };
    const variantFields = ["count", "enabled", "data", "values", "scalar", "when", "role", "children", "byName", "note"];
    for (const fieldName of variantFields) {
      const prefixedKey = Object.keys(normalized).find((key) => key.endsWith(`__${fieldName}`));
      if (prefixedKey) {
        normalized[fieldName] = normalized[prefixedKey];
      }
    }

    return normalized;
  }

  return value;
}, DetailsDiscriminated);

export type DetailsWrite =
  | { _type: "Note"; count: number; enabled: boolean }
  | { _type: "Empty" }
  | { _type: "Raw"; data: unknown; values: Array<number | null>; scalar: string | null }
  | { _type: "Bundle"; when: Date; role: RoleJsonInput; children: Array<DetailsJsonInput>; byName: Record<string, DetailsJsonInput>; note: string | null }
;
export const DetailsWrite: z.ZodType<DetailsWrite> = z.discriminatedUnion('_type', [
  z.strictObject({ _type: z.literal("Note"),
    count: z.number().int(),
    enabled: z.boolean(),
  }),
  z.strictObject({ _type: z.literal("Empty"),
  }),
  z.strictObject({ _type: z.literal("Raw"),
    data: z.json().refine(value => value !== null).nonoptional(),
    values: z.array(z.number().int().nullable()),
    scalar: z.string().nullable(),
  }),
  z.strictObject({ _type: z.literal("Bundle"),
    when: CoercedDate,
    role: z.lazy(() => RoleJsonInput),
    children: z.array(z.lazy(() => DetailsJsonInput)),
    byName: z.record(z.string(), z.lazy(() => DetailsJsonInput)),
    note: z.string().nullable(),
  }),
]);

export const DetailsJsonInput = DetailsWrite;
export type DetailsJsonInput = DetailsWrite;

export type DetailsSessionInput =
  | { _type: "Note"; count: number; enabled: boolean }
  | { _type: "Empty" }
  | { _type: "Raw"; data: unknown; values: Array<number | null>; scalar: string | null }
  | { _type: "Bundle"; when: Date; role: RoleSessionInput; children: Array<DetailsSessionJsonInput>; byName: Record<string, DetailsSessionJsonInput>; note?: string | null }
;
export const DetailsSessionInput: z.ZodType<DetailsSessionInput> = z.discriminatedUnion('_type', [
  z.object({ _type: z.literal("Note"),
    count: z.number().int(),
    enabled: CoercedBool,
  }),
  z.object({ _type: z.literal("Empty"),
  }),
  z.object({ _type: z.literal("Raw"),
    data: z.json().refine(value => value !== null).nonoptional(),
    values: z.array(z.number().int().nullable()),
    scalar: z.string().nullable(),
  }),
  z.object({ _type: z.literal("Bundle"),
    when: CoercedDate,
    role: z.lazy(() => RoleSessionInput),
    children: z.array(z.lazy(() => DetailsSessionJsonInput)),
    byName: z.record(z.string(), z.lazy(() => DetailsSessionJsonInput)),
    note: z.string().nullish(),
  }),
]);

export type DetailsSessionJsonInput =
  | { _type: "Note"; count: number; enabled: boolean }
  | { _type: "Empty" }
  | { _type: "Raw"; data: unknown; values: Array<number | null>; scalar: string | null }
  | { _type: "Bundle"; when: Date; role: RoleSessionJsonInput; children: Array<DetailsSessionJsonInput>; byName: Record<string, DetailsSessionJsonInput>; note?: string | null }
;
export const DetailsSessionJsonInput: z.ZodType<DetailsSessionJsonInput> = z.discriminatedUnion('_type', [
  z.object({ _type: z.literal("Note"),
    count: z.number().int(),
    enabled: CoercedBool,
  }),
  z.object({ _type: z.literal("Empty"),
  }),
  z.object({ _type: z.literal("Raw"),
    data: z.json().refine(value => value !== null).nonoptional(),
    values: z.array(z.number().int().nullable()),
    scalar: z.string().nullable(),
  }),
  z.object({ _type: z.literal("Bundle"),
    when: CoercedDate,
    role: z.lazy(() => RoleSessionJsonInput),
    children: z.array(z.lazy(() => DetailsSessionJsonInput)),
    byName: z.record(z.string(), z.lazy(() => DetailsSessionJsonInput)),
    note: z.string().nullish(),
  }),
]);

// Session type
export interface Session {
  userId: number;
  role: Role;
  unrelated: string;
  context?: Details | null;
}

const EffectiveSessionValidator = z.object({
  userId: z.number().int(),
  role: z.lazy(() => RoleSessionInput),
  unrelated: z.string(),
  context: z.lazy(() => DetailsSessionJsonInput).nullish(),
});

export const SessionValidator = z.preprocess((value, ctx) => {
  const parsed = EffectiveSessionValidator.safeParse(value);
  if (!parsed.success) {
    for (const issue of parsed.error.issues) ctx.addIssue({ code: 'custom', path: issue.path, message: issue.message });
    return z.NEVER;
  }
  return parsed.data;
}, z.object({
  userId: z.number().int(),
  role: z.lazy(() => Role),
  unrelated: z.string(),
  context: z.lazy(() => Details).nullish(),
}));

