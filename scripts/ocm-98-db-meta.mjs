import { DatabaseSync } from "node:sqlite";

const [databasePath, field] = process.argv.slice(2);
if (!databasePath) {
  throw new Error("usage: node scripts/ocm-98-db-meta.mjs <database> [field]");
}

const database = new DatabaseSync(databasePath, { readOnly: true });
try {
  const userVersionRow = database.prepare("PRAGMA user_version").get();
  const metadata = database
    .prepare(
      "SELECT schema_version AS schemaVersion, app_version AS appVersion, created_at AS createdAt, updated_at AS updatedAt FROM schema_meta WHERE meta_key = 'primary'",
    )
    .get();
  const result = {
    userVersion: Number(userVersionRow?.user_version ?? 0),
    schemaVersion: Number(metadata?.schemaVersion ?? 0),
    appVersion: metadata?.appVersion ?? null,
    createdAt: metadata?.createdAt ?? null,
    updatedAt: metadata?.updatedAt ?? null,
  };
  if (field) {
    if (!(field in result)) {
      throw new Error(`unknown field: ${field}`);
    }
    process.stdout.write(`${result[field] ?? "null"}\n`);
  } else {
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  }
} finally {
  database.close();
}
