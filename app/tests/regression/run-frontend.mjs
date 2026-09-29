import { spawnSync } from 'node:child_process';
const cases = [
  'snapshot-ids', 'draft-effect', 'new-session-race', 'resume-session-race',
  'failed-new-keeps-history', 'empty-rebind', 'snapshot-sid-and-write-order',
  'loading-owner-and-stale-read', 'permission-double-submit', 'elicitation-double-submit',
  'session-event-routing', 'pending-config-is-per-context', 'invoke-result-before-final-events', 'stale-prompt-finally', 'ready-caps-and-first-message',
  'content-only-tool', 'image-cross-context', 'delayed-image-reader',
  'unbind-clears-snapshot', 'history-bind-race', 'default-full-access-unchanged',
];
let failed = 0;
for (const name of cases) {
  const result = spawnSync(process.execPath, ['--conditions=browser', 'frontend-tests.mjs', name], { stdio: 'inherit' });
  if (result.status !== 0) failed++;
}
console.log(`Frontend: ${cases.length - failed} passed, ${failed} failed (${cases.length} cases).`);
process.exitCode = failed ? 1 : 0;
