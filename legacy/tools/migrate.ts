#!/usr/bin/env bun
/**
 * Migration harness for the Rust port's schema-parity test.
 *
 * Applies one of the TypeScript migration ladders to an existing SQLite file so
 * the Rust test can diff the resulting schema against its own. Temporary
 * scaffolding: it is removed together with the rest of legacy/ once the port
 * lands.
 *
 *   bun legacy/tools/migrate.ts <db-path> <historical|debug>
 */
import { SQL } from "bun";
import {
	DEBUG_MIGRATIONS,
	HISTORICAL_MIGRATIONS,
	migrate,
} from "../src/migrations";

const [dbPath, ladder] = process.argv.slice(2);

if (!dbPath || (ladder !== "historical" && ladder !== "debug")) {
	console.error(
		"usage: bun legacy/tools/migrate.ts <db-path> <historical|debug>",
	);
	process.exit(2);
}

const sql = new SQL(`sqlite://${dbPath}`);
try {
	await migrate(
		sql,
		ladder === "historical" ? HISTORICAL_MIGRATIONS : DEBUG_MIGRATIONS,
		ladder,
	);
} finally {
	await sql.close();
}
