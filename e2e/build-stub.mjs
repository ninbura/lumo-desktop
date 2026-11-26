import { build } from 'esbuild';
import { execFileSync } from 'node:child_process';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..');
const clients = resolve(process.env.CLIENTS_DIR || join(root, '../clients'));
const out = join(root, 'src-tauri/dist');
const embedded = process.argv.includes('--embedded');
const ref = execFileSync('git', ['-C', clients, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
console.log(`Bundling stub against clients ${ref} (${clients})`);
await mkdir(out, { recursive: true });
await build({
    entryPoints: [join(clients, 'packages/lumo-api-client/e2e/desktop-bridge-harness.ts')],
    alias: { react: join(root, 'node_modules/react') },
    bundle: true,
    format: 'iife',
    target: 'es2020',
    outfile: join(out, 'e2e_testing_stub.js'),
});
let html = await readFile(join(here, 'e2e_testing_stub.html'), 'utf8');
if (embedded) {
    await build({
        stdin: { contents: "import '@wdio/tauri-plugin';", resolveDir: here },
        bundle: true,
        format: 'iife',
        target: 'es2020',
        outfile: join(out, 'e2e_wdio_init.js'),
    });
    html = html.replace('<script src="e2e_testing_stub.js">', '<script src="e2e_wdio_init.js"></script>\n    <script src="e2e_testing_stub.js">');
}
await writeFile(join(out, 'e2e_testing_stub.html'), html);
