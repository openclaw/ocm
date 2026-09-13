import { DatabaseSync } from "node:sqlite";

const [databasePath, field] = process.argv.slice(2);
if (!databasePath) {
  throw new Error("usage: node scripts/ocm-98-db-meta.mjs <database> [field]");
}

const retireCommitmentsV7 = field === "retireCommitmentsV7";
const database = new DatabaseSync(databasePath, { readOnly: !retireCommitmentsV7 });
try {
  if (retireCommitmentsV7) {
    const userVersion = Number(database.prepare("PRAGMA user_version").get()?.user_version ?? 0);
    const metadata = database
      .prepare("SELECT schema_version AS schemaVersion FROM schema_meta WHERE meta_key = 'primary'")
      .get();
    if (userVersion !== 6 || Number(metadata?.schemaVersion ?? 0) !== 6) {
      throw new Error(
        `expected an exact schema 6 database before the schema 7 retirement; got user_version=${userVersion}, schema_meta=${metadata?.schemaVersion ?? "missing"}`,
      );
    }
    const expectedObjects = [
      "commitments",
      "idx_commitments_scope_due",
      "idx_commitments_status_due",
      "idx_commitments_scope_dedupe",
      "idx_commitments_agent_due",
      "idx_commitments_agent_sent",
    ];
    const actualObjects = database
      .prepare(
        `SELECT name FROM sqlite_schema
         WHERE name = 'commitments' OR name LIKE 'idx_commitments_%'
         ORDER BY name`,
      )
      .all()
      .map((row) => row.name);
    if (JSON.stringify(actualObjects) !== JSON.stringify([...expectedObjects].sort())) {
      throw new Error(
        `schema 6 commitments objects did not match the canonical set: ${JSON.stringify(actualObjects)}`,
      );
    }
    database.exec(`
      BEGIN IMMEDIATE;
      DROP TABLE commitments;
      PRAGMA user_version = 7;
      UPDATE schema_meta
      SET schema_version = 7,
          updated_at = unixepoch('now') * 1000
      WHERE meta_key = 'primary';
      COMMIT;
    `);
    process.stdout.write("retired canonical schema 6 commitments storage; schema is now 7\n");
  } else {
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
  }
} finally {
  database.close();
}
