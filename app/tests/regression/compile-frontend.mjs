// Compile the audited source without editing the checkout. Native IPC and visual-only
// child components are stubbed; ChatView/state/permission logic is unchanged.
import fs from 'node:fs/promises';
import path from 'node:path';
import { createRequire } from 'node:module';
const root = process.env.SHIDRIVE_APP ?? path.resolve('../..');
const require = createRequire(root + '/package.json');
const { compile, compileModule } = require('svelte/compiler');
const ts = require('typescript');
const out = path.resolve('compiled');
await fs.mkdir(out + '/components', { recursive: true });
await fs.writeFile(out + '/package.json', '{"type":"module"}');
await fs.symlink(root + '/node_modules/svelte', path.resolve('node_modules/svelte')).catch(() => {});
function js(source) {
  return ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } }).outputText;
}
function imports(code) {
  return code.replaceAll('"./ipc"', '"./ipc.js"').replaceAll('"./theme"', '"./theme.js"')
    .replaceAll('"./state.svelte"', '"./state.svelte.js"')
    .replaceAll('"../state.svelte"', '"../state.svelte.js"')
    .replaceAll('"../dialog.svelte"', '"../dialog.svelte.js"')
    .replaceAll('"../markdown"', '"../markdown.js"')
    .replaceAll('"@tauri-apps/api/event"', '"./tauri-event.js"')
    .replaceAll('"../ipc"', '"../ipc.js"').replaceAll('"../i18n"', '"../i18n.js"')
    .replace(/(["']\.\/\w+\.svelte)(["'])/g, '$1.js$2')
    .replace('import.meta.env.DEV', 'false');
}
const state = await fs.readFile(root + '/src/lib/state.svelte.ts', 'utf8');
await fs.writeFile(out + '/state.svelte.js', imports(compileModule(js(state), { filename: 'state.svelte.js', generate: 'client', dev: true }).js.code));
for (const name of ['ChatView', 'PermissionDialog', 'ElicitationDialog', 'HistoryBindModal', 'SessionManagerModal', 'MessageItem']) {
  let source = await fs.readFile(root + '/src/lib/components/' + name + '.svelte', 'utf8');
  const accessors = {
    ChatView: `return { newSession, resumeSession, send, setCfg, pasteIntoInput, onPaste, removeImage, getState: () => ({key, input, stickToBottom, bindingTitle, pendingImages}), addImage: (img) => addDraftImage(key,img), setStick: v => stickToBottom=v };`,
    PermissionDialog: `return { respond };`, ElicitationDialog: `return { respond };`,
    HistoryBindModal: `return { bind };`, SessionManagerModal: `return { unbind };`,
    MessageItem: `return { toolDetail };`,
  };
  const exportText = `export function audit() { ${accessors[name]} }`;
  source = source.replace('</script>', exportText + '\n</script>');
  const compiled = compile(source, { filename: name + '.svelte', generate: 'client', dev: true });
  await fs.writeFile(out + '/components/' + name + '.svelte.js', imports(compiled.js.code));
}
for (const name of ['ChatTimeline', 'ContextMenu', 'Icon']) {
  const text = name === 'MessageItem' ? '<script>let {item}=$props();</script><p>{item.text}</p>' : '<span></span>';
  await fs.writeFile(out + '/components/' + name + '.svelte.js', compile(text, { filename: name + '.svelte', generate: 'client', dev: true }).js.code);
}
await fs.writeFile(out + '/ipc.js', 'export const api = new Proxy({}, {get: (_o, method) => (...args) => globalThis.__api(method, args)});');
await fs.writeFile(out + '/theme.js', 'export function applyTheme() {}');
await fs.writeFile(out + '/i18n.js', 'export const localeTag = () => "zh-CN"; export function t(s, values={}) { return String(s).replace(/\\{([^}]+)\\}/g, (_, k) => String(values[k] ?? k)); }');
await fs.writeFile(out + '/dialog.svelte.js', 'export const confirmDialog = (...a) => globalThis.__confirm(...a); export const promptDialog = async () => null;');
await fs.writeFile(out + '/events.js', imports(js(await fs.readFile(root + '/src/lib/events.ts', 'utf8'))));
await fs.writeFile(out + '/tauri-event.js', 'export const listen = async (name, fn) => { globalThis.__listeners.set(name, fn); return () => globalThis.__listeners.delete(name); };');
await fs.writeFile(out + '/markdown.js', 'export const md = s => String(s); export const localLinkPath = () => null;');
console.log('Compiled real state.svelte.ts + ChatView/PermissionDialog/ElicitationDialog with IPC/visual stubs.');
