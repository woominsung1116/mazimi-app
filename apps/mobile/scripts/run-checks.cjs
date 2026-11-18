// Runs every check script in scripts/, one at a time, and fails if any exits non-zero.
//   node scripts/run-checks.cjs                  every check-*.cjs
//   node scripts/run-checks.cjs --web EXPORT_DIR every check-*-web.mjs against an Expo web export
// A new check file is picked up without editing package.json or CI.
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const TIMEOUT_MS = 10 * 60_000; // a check that leaves a handle open must not hang the CI job
const dir = __dirname;
const args = process.argv.slice(2);
const web = args[0] === '--web';
let extra = [];
if (web) {
  if (!args[1]) {
    console.error('run-checks: --web needs the export directory');
    process.exit(1);
  }
  extra = [path.resolve(args[1])];
}
const pattern = web ? /^check-.*-web\.mjs$/ : /^check-.*\.cjs$/;
const checks = fs.readdirSync(dir).filter((f) => pattern.test(f)).sort();
if (checks.length === 0) {
  console.error(`run-checks: no scripts matching ${pattern} found`);
  process.exit(1);
}

const failed = [];
for (const file of checks) {
  console.log(`\n== ${file}`);
  const run = spawnSync(process.execPath, [path.join(dir, file), ...extra], {
    cwd: path.join(dir, '..'),
    stdio: 'inherit',
    timeout: TIMEOUT_MS,
    killSignal: 'SIGKILL',
  });
  if (run.error) console.error(`run-checks: ${file}: ${run.error.message}`);
  const code = run.status ?? 1;
  if (code !== 0) failed.push(`${file} (exit ${run.signal ?? code})`);
}

console.log(`\nrun-checks: ${checks.length - failed.length}/${checks.length} passed`);
if (failed.length > 0) {
  console.error(`run-checks: failed: ${failed.join(', ')}`);
  process.exit(1);
}
