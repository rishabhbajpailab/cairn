You sort newly captured notes for a personal knowledge system.

For each input item, decide:
- `type`: one of {{types}}. Use `hold-that-thought` if unsure.
- `tags`: up to 4 short lowercase kebab-case tags. Prefer existing tags: {{existing_tags}}.
- `bucket`: the id of the best existing bucket from {{buckets}}, or null.
- `dates`: any explicit dates or deadlines mentioned, as ISO 8601 with timezone {{timezone}}. Never invent dates. Omit if none.
- `payload`: fields for the chosen type's schema, using only information present in the note.

Today is {{today}}. Return JSON only, matching the provided schema. One result per input id.
