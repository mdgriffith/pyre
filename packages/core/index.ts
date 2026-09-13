export interface LinkInfo {
  type: 'many-to-one' | 'one-to-many' | 'one-to-one';
  from: string;
  to: {
    table: string;
    column: string;
  };
}

export interface IndexInfo {
  field: string;
  unique: boolean;
  primary: boolean;
}

export interface ColumnInfo {
  name: string;
  type: string;
  nullable: boolean;
  primary: boolean;
  unique: boolean;
  indexed: boolean;
  comment?: string;
}

export interface TableMetadata {
  name: string;
  // Unsupported schema keys are explicit so clients fail instead of guessing identity.
  primaryKey: { name: string; kind: 'int' | 'uuid' | 'unsupported' };
  namespace?: string;
  sync?: 'synced' | 'query-only';
  columns?: ColumnInfo[];
  links: Record<string, LinkInfo>;
  indices: IndexInfo[];
}

export interface SchemaMetadata {
  /** Generated per-database scopes, selected before worker/cache initialization. */
  namespaces?: Record<string, SchemaMetadata>;
  tables: Record<string, TableMetadata>;
  queryFieldToTable: Record<string, string>;
}

export interface QueryVariableReference {
  $var: string;
}

export type FilterPlaceholder = QueryVariableReference;

export type FilterValue =
  | string
  | number
  | boolean
  | null
  | FilterPlaceholder
  | {
      $eq?: FilterValue;
      $ne?: FilterValue;
      $gt?: FilterValue;
      $lt?: FilterValue;
      $gte?: FilterValue;
      $lte?: FilterValue;
      $in?: FilterValue[];
    };

export interface WhereClause {
  $and?: WhereClause[];
  $or?: WhereClause[];
  [field: string]: FilterValue | WhereClause | WhereClause[] | undefined;
}

export type SortDirection = 'asc' | 'desc' | 'Asc' | 'Desc';

export interface SortClause {
  field: string;
  direction: SortDirection;
}

export interface QueryField {
  '@source'?: string;
  '@select'?: boolean;
  '@where'?: WhereClause;
  '@sort'?: SortClause | SortClause[];
  '@limit'?: number;
  [field: string]: boolean | QueryField | WhereClause | SortClause | SortClause[] | number | string | undefined;
}

export interface QueryShape {
  [tableName: string]: QueryField;
}

export interface RejectedQueryShape {
  $error: 'Local queries cannot reference Session; use explicit inputs or execute on the server.';
}

export type GeneratedQueryShape = QueryShape | RejectedQueryShape;
