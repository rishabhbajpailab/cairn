// Mirrors crates/cairn-domain. Keep in sync with docs/data-model.md.

export type Lifecycle = 'active' | 'archived' | 'trashed';

export interface Item {
  id: string;
  type: string;
  type_version: number;
  title: string;
  payload: Record<string, unknown>;
  idx: Record<string, unknown>;
  tags: string[];
  lifecycle: Lifecycle;
  pinned: boolean;
  source: { kind: string; ref?: string; label?: string };
  created_at: string;
  updated_at: string;
}

export interface Edge {
  id: string;
  from: string;
  to: string;
  kind: string;
  props: Record<string, unknown>;
  created_at: string;
}

export interface Query {
  types?: string[];
  tags_all?: string[];
  tags_any?: string[];
  tags_none?: string[];
  lifecycle?: Lifecycle[];
  where?: { field: string; op: string; value: unknown }[];
  text?: string;
  related_to?: { item: string; kind?: string; depth?: number };
  updated_since?: string;
  sort?: { field: string; dir?: 'asc' | 'desc' }[];
  limit?: number;
}

export interface TypeInfo {
  name: string;
  version: number;
  title: string;
  icon?: string;
  index: Record<string, string>;
  actions: Record<string, { discord_button?: string }>;
  views?: string;
}

export const EDGE_KINDS = ['relates', 'contains', 'blocks', 'derived-from', 'remind-about'] as const;
