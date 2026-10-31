import { getConfig } from "./config";
import { buildQuery, type SqlQuery } from "./query";
import { runMockQuery } from "./mock";

export interface SqlRow {
  [column: string]: unknown;
}

async function runHttpQuery(query: SqlQuery): Promise<unknown[]> {
  const { baseUrl, token } = getConfig();
  const response = await fetch(`${baseUrl}/v1/sql`, {
    method: "POST",
    credentials: "include",
    headers: {
      "Content-Type": "application/json",
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    body: JSON.stringify(query),
  });

  if (!response.ok) {
    throw new Error(`db.sql request failed: ${response.status} ${response.statusText}`);
  }

  const data = await response.json();
  return Array.isArray(data) ? data : (data.rows ?? []);
}

function sqlTag<T = SqlRow>(strings: TemplateStringsArray, ...values: unknown[]): Promise<T[]> {
  const query = buildQuery(strings, values);
  const { mode } = getConfig();
  const rows = mode === "mock" ? runMockQuery(query) : runHttpQuery(query);
  return rows as Promise<T[]>;
}

export const db = {
  sql: sqlTag,
};

export interface LulaUser {
  id: string;
  name: string;
  email: string;
}

const MOCK_USER: LulaUser = {
  id: "demo-user",
  name: "Demo User",
  email: "demo@example.com",
};

export async function currentUser(): Promise<LulaUser> {
  const { baseUrl, mode, token } = getConfig();
  if (mode === "mock") {
    return { ...MOCK_USER };
  }

  const response = await fetch(`${baseUrl}/v1/me`, {
    method: "GET",
    credentials: "include",
    headers: token ? { Authorization: `Bearer ${token}` } : undefined,
  });

  if (!response.ok) {
    throw new Error(`currentUser request failed: ${response.status} ${response.statusText}`);
  }

  return response.json();
}
