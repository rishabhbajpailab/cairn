import type { Edge, Item, Query, TypeInfo } from './types';

// The web UI sits behind the GitHub sign-in (ADR 0015); the API still wants a write-scoped token.
// Browser storage is a per-browser convenience only, so guard every access.
const TOKEN_KEY = 'cairn.token';

export function getToken(): string | null {
  try {
    return localStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

export function setToken(t: string) {
  try {
    localStorage.setItem(TOKEN_KEY, t);
  } catch {
    /* storage unavailable: token lives for this page only */
  }
  memoryToken = t;
}

export function clearToken() {
  try {
    localStorage.removeItem(TOKEN_KEY);
  } catch {
    /* nothing stored */
  }
  memoryToken = null;
}

/** Per-browser flags (e.g. "checklist already offered"). Never relied on for correctness. */
export function flag(name: string, set?: boolean): boolean {
  const key = `cairn.${name}`;
  try {
    if (set !== undefined) localStorage.setItem(key, set ? '1' : '');
    return localStorage.getItem(key) === '1';
  } catch {
    return !!set;
  }
}

let memoryToken: string | null = null;

export class ApiError extends Error {
  constructor(public status: number, public code: string, message: string) {
    super(message);
  }
}

async function call<T>(method: string, path: string, body?: unknown, contentType = 'application/json'): Promise<T> {
  const token = memoryToken ?? getToken();
  const res = await fetch(path, {
    method,
    headers: {
      ...(token ? { authorization: `Bearer ${token}` } : {}),
      ...(body !== undefined ? { 'content-type': contentType } : {}),
    },
    body: body === undefined ? undefined : contentType === 'application/json' ? JSON.stringify(body) : String(body),
  });
  if (res.status === 204) return undefined as T;
  const json = await res.json().catch(() => null);
  if (!res.ok) throw new ApiError(res.status, json?.error?.code ?? 'http', json?.error?.message ?? res.statusText);
  return json as T;
}

export const api = {
  capture: (text: string) => call<{ capture_id: string; item_id: string | null; type: string | null }>('POST', '/api/v1/capture', text, 'text/plain'),
  query: (q: Query) => call<Item[]>('POST', '/api/v1/query', q),
  item: (id: string) => call<Item>('GET', `/api/v1/items/${id}`),
  patch: (id: string, payload: Record<string, unknown>) => call<Item>('PATCH', `/api/v1/items/${id}`, { payload }),
  lifecycle: (id: string, to: 'active' | 'archived' | 'trashed') => call<void>('POST', `/api/v1/items/${id}/lifecycle`, { to }),
  edges: (id: string) => call<Edge[]>('GET', `/api/v1/items/${id}/edges`),
  link: (from: string, to: string, kind: string) => call<Edge>('POST', '/api/v1/edges', { from, to, kind }),
  types: () => call<{ types: TypeInfo[] }>('GET', '/api/v1/types'),
  /** Seed the "Getting started" checklist; a no-op if one was ever seeded. */
  onboard: () => call<{ created: boolean; bucket: string | null; steps: number }>('POST', '/api/v1/onboarding'),
};
