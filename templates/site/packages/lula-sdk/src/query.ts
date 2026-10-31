export interface SqlQuery {
  sql: string;
  params: unknown[];
}

/**
 * Turns a tagged-template call into a parameterized query: interpolated
 * values always become `?` placeholders + a params array, never inlined
 * text, so callers can't accidentally build an injectable query.
 */
export function buildQuery(strings: TemplateStringsArray, values: unknown[]): SqlQuery {
  let sql = strings[0] ?? "";
  for (let i = 0; i < values.length; i++) {
    sql += "?" + (strings[i + 1] ?? "");
  }
  return { sql, params: values };
}
