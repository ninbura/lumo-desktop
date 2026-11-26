#!/usr/bin/env node
//
// Builds tools-helper/ into src-tauri/target/tools-helper/Lumo Tools.app, which
// tauri.conf.json copies into Lumo.app/Contents/Helpers. No-op for non-macOS targets.
//
// The bundle is signed ad hoc so that its signature records its own identifier and no
// entitlements. Tauri does not sign copied files, and the CI signer preserves both values.
//
// LUMO_TOOLS_LAUNCH_CONSTRAINT adds tools-helper/launch-constraint.plist, under which macOS
// starts the helper only as a child of the Developer ID signed Lumo. Unset for local builds.

import { execFileSync } from 'node:child_process'
import { copyFileSync, mkdirSync, rmSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const triple = process.env.TAURI_ENV_TARGET_TRIPLE ?? ''
if (!triple.endsWith('-apple-darwin')) {
    process.exit(0)
}

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const crateDir = join(repoRoot, 'tools-helper')
const appDir = join(repoRoot, 'src-tauri', 'target', 'tools-helper', 'Lumo Tools.app')
const binary = join(appDir, 'Contents', 'MacOS', 'lumo-tools')
const run = (command, args) => execFileSync(command, args, { stdio: 'inherit' })

const targets =
    triple === 'universal-apple-darwin' ? ['aarch64-apple-darwin', 'x86_64-apple-darwin'] : [triple]
const built = targets.map((target) => {
    run('cargo', ['build', '--release', '--locked', '--manifest-path', join(crateDir, 'Cargo.toml'), '--target', target])
    return join(crateDir, 'target', target, 'release', 'lumo-tools')
})

rmSync(appDir, { recursive: true, force: true })
mkdirSync(dirname(binary), { recursive: true })
copyFileSync(join(crateDir, 'Info.plist'), join(appDir, 'Contents', 'Info.plist'))
if (built.length > 1) {
    run('lipo', ['-create', '-output', binary, ...built])
} else {
    copyFileSync(built[0], binary)
}
const launchConstraint = process.env.LUMO_TOOLS_LAUNCH_CONSTRAINT
    ? ['--launch-constraint-parent', join(crateDir, 'launch-constraint.plist'), '--enforce-constraint-validity']
    : []
run('codesign', ['--force', '--sign', '-', ...launchConstraint, appDir])
