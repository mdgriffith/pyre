import { z } from 'zod';

declare const EntryIdBrand: unique symbol;
export type EntryId = string & { readonly [EntryIdBrand]: true };
export const EntryId = z.string().length(36).regex(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i).transform((value): EntryId => value as EntryId);

