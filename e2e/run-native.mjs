import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { seedFixtures } from './support/fixtures.js';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..');
const windows = process.platform === 'win32';
const targetDir = resolve(process.env.CARGO_TARGET_DIR || join(root, 'src-tauri/target/e2e'));
const artifactDir = resolve(process.env.LUMO_E2E_ARTIFACT_DIR || tmpdir());
const args = process.argv.slice(2);
const requested = args.filter((arg) => arg !== '--skip-build');
const available = (await readdir(join(here, 'specs'))).filter((name) => name.endsWith('.e2e.js')).sort();
for (const name of requested) {
    if (!available.includes(name)) throw new Error(`Unknown spec ${name}; pass filenames such as discovery.e2e.js`);
}
const specs = requested.length ? requested : available;
const timeout = Number(process.env.LUMO_E2E_TIMEOUT || 600) * 1000;
if (!Number.isFinite(timeout) || timeout <= 0) throw new Error('LUMO_E2E_TIMEOUT must be positive seconds');

function run(command, argv, cwd, env = process.env, bounded = false) {
    return new Promise((resolveRun, reject) => {
        const child = spawn(command, argv, { cwd, env, stdio: 'inherit', detached: !windows });
        const stop = () => {
            if (windows) spawn('taskkill', ['/pid', String(child.pid), '/T', '/F']);
            else { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }
        };
        const interrupt = () => { stop(); };
        process.once('SIGINT', interrupt);
        process.once('SIGTERM', interrupt);
        let expired = false;
        const timer = bounded ? setTimeout(() => { expired = true; stop(); }, timeout) : undefined;
        const clean = () => {
            clearTimeout(timer);
            process.removeListener('SIGINT', interrupt);
            process.removeListener('SIGTERM', interrupt);
        };
        child.once('error', (error) => { clean(); reject(error); });
        child.once('exit', (code, signal) => {
            clean();
            if (code === 0 && !expired) resolveRun();
            else reject(new Error(`${command} failed (${expired ? 'timeout' : code ?? signal})`));
        });
    });
}

/**
 * tauri-build regenerates the tracked src-tauri/gen/schemas from the plugins compiled in, so
 * the e2e-webdriver build writes the test-only wdio permissions into them. A later normal
 * build does not reliably undo that (cargo skips the build script when its inputs are
 * unchanged), so put back exactly what was there before, even when the build fails.
 */
async function preservingSchemas(build) {
    const dir = join(root, 'src-tauri/gen/schemas');
    const list = async () => (await readdir(dir).catch(() => [])).filter((name) => name.endsWith('.json'));
    const saved = new Map();
    for (const name of await list()) saved.set(name, await readFile(join(dir, name)));
    try {
        await build();
    } finally {
        for (const name of await list()) {
            if (!saved.has(name)) await rm(join(dir, name), { force: true });
        }
        for (const [name, data] of saved) await writeFile(join(dir, name), data);
    }
}

if (!args.includes('--skip-build')) {
    await run(process.execPath, [join(here, 'build-stub.mjs'), '--embedded'], root);
    await run(process.execPath, [join(root, 'scripts/build-webview.mjs')], root);
    // Separate artifacts keep ordinary app launches free of automation.
    await preservingSchemas(() => run('cargo', ['build', '--locked', '--features', 'e2e-webdriver',
        '--bin', 'lumo-desktop', '--target-dir', targetDir], join(root, 'src-tauri'),
        { ...process.env, CARGO_PROFILE_DEV_DEBUG: '0', CARGO_INCREMENTAL: '0' }));
}
const binary = resolve(process.env.LUMO_APP_BINARY || join(targetDir, 'debug', `lumo-desktop${windows ? '.exe' : ''}`));
await mkdir(artifactDir, { recursive: true });
for (const spec of specs) {
    // A whole WDIO invocation per spec guarantees a fresh native process and stub.
    const dir = await mkdtemp(join(artifactDir, 'lumo-e2e-'));
    const env = { ...process.env, ...seedFixtures(dir), LUMO_APP_BINARY: binary, LUMO_E2E_LOG_DIR: join(dir, 'logs'),
        LUMO_E2E_EXPECT_SECURE_STORAGE: process.env.LUMO_E2E_EXPECT_SECURE_STORAGE ||
            (process.platform === 'linux' ? 'unavailable' : 'available') };
    console.log(`\nRunning ${spec} (state: ${dir})`);
    try {
        await run(process.execPath, [join(here, 'node_modules/@wdio/cli/bin/wdio.js'),
            'run', join(here, 'wdio.conf.js'), '--spec', join(here, 'specs', spec)], here, env, true);
    } catch (error) {
        console.error(`Failure artifacts retained at ${dir}`);
        throw error;
    }
    await rm(dir, { recursive: true, force: true });
}
