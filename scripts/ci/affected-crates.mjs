import { spawnSync } from 'node:child_process'
import path from 'node:path'

/**
 * @typedef {{ name: string, directory: string, dependencies: string[] }} WorkspaceCrate
 * @typedef {{ root: string, crates: WorkspaceCrate[] }} Workspace
 * @typedef {{ file: string, crate: string }} ChangedFile
 * @typedef {{ full: boolean, reasons: string[], changed: string[], crates: string[] }} AffectedResult
 * @typedef {{ base: string, check: boolean, args: boolean }} AffectedOptions
 * @typedef {{ crate: string, missing: string[], extra: string[] }} CrateDifference
 * @typedef {{ name: string, path?: string }} MetadataDependency
 * @typedef {{ id: string, name: string, manifest_path: string, dependencies: MetadataDependency[] }} MetadataPackage
 * @typedef {{ workspace_root: string, workspace_members: string[], packages: MetadataPackage[] }} CargoMetadata
 */

export class AffectedCrates {
    static DEFAULT_BASE = 'origin/develop'
    static FULL = 'full'
    static NONE = 'none'
    static WORKSPACE_FLAG = '--workspace'
    static PACKAGE_FLAG = '-p'
    static KNOWN_FLAGS = ['--base', '--check', '--args']
    static FULL_NAMES = ['Cargo.toml', 'Cargo.lock', 'build.rs']
    static FULL_FILES = [
        'rust-toolchain.toml',
        'rust-miri-toolchain.toml',
        'rustfmt.toml',
        'clippy.toml',
        '.clippy.toml',
        'deny.toml'
    ]
    static FULL_PREFIXES = ['.cargo/', '.config/', 'crates/fylo-test-support/']
    static RUST_SUFFIX = '.rs'
    static RUST_FILES = ['VERSION']
    static RUST_PREFIXES = ['tests/fixtures/', 'vendor/', 'fuzz/']
    static METADATA_ARGUMENTS = ['metadata', '--format-version', '1', '--no-deps', '--locked']
    static TREE_ARGUMENTS = [
        'tree',
        '--locked',
        '--workspace',
        '--all-features',
        '--target',
        'all',
        '--edges',
        'normal,build,dev',
        '--prefix',
        'none',
        '--format',
        '{p}',
        '--invert'
    ]
    static OUTPUT_LIMIT = 64 * 1024 * 1024
    static LOG_PREFIX = 'affected-crates:'

    /**
     * @param {string[]} argv
     * @returns {void}
     */
    static main(argv) {
        const options = AffectedCrates.#options(argv)
        const workspace = AffectedCrates.readWorkspace(process.cwd())
        const files = AffectedCrates.changedFiles(workspace.root, options.base)
        const result = AffectedCrates.affected(workspace, files)
        AffectedCrates.#logSummary(options, files, result)
        AffectedCrates.check(workspace, AffectedCrates.#checkedCrates(options, result))
        console.log(
            AffectedCrates.#modeText(
                options,
                AffectedCrates.listText(result),
                AffectedCrates.argumentText(result)
            )
        )
    }

    /**
     * @param {string} directory
     * @returns {Workspace}
     */
    static readWorkspace(directory) {
        const text = AffectedCrates.#run('cargo', AffectedCrates.METADATA_ARGUMENTS, directory)
        return AffectedCrates.workspaceFromMetadata(JSON.parse(text))
    }

    /**
     * @param {CargoMetadata} metadata
     * @returns {Workspace}
     */
    static workspaceFromMetadata(metadata) {
        const members = AffectedCrates.#memberPackages(metadata)
        const names = members.map((member) => member.name)
        const crates = members.map((member) =>
            AffectedCrates.#crateFrom(metadata.workspace_root, member, names)
        )
        return { root: metadata.workspace_root, crates: AffectedCrates.#deepestFirst(crates) }
    }

    /**
     * @param {string} root
     * @param {string} base
     * @returns {string[]}
     */
    static changedFiles(root, base) {
        const mergeBase = AffectedCrates.#git(root, ['merge-base', base, 'HEAD']).trim()
        const tracked = AffectedCrates.#git(root, [
            'diff',
            '--name-only',
            '--no-renames',
            '--relative',
            mergeBase
        ])
        const untracked = AffectedCrates.#git(root, ['ls-files', '--others', '--exclude-standard'])
        return AffectedCrates.#unique([
            ...AffectedCrates.#lines(tracked),
            ...AffectedCrates.#lines(untracked)
        ])
    }

    /**
     * @param {Workspace} workspace
     * @param {string[]} files
     * @returns {AffectedResult}
     */
    static affected(workspace, files) {
        const owned = files.map((file) => AffectedCrates.#ownedFile(workspace, file))
        const changed = AffectedCrates.#unique(AffectedCrates.#ownerNames(owned))
        const reasons = [
            ...AffectedCrates.#sharedFiles(files),
            ...AffectedCrates.#rustFiles(AffectedCrates.#looseFiles(owned))
        ]
        return {
            full: reasons.length > 0,
            reasons: AffectedCrates.#unique(reasons),
            changed,
            crates: AffectedCrates.reverseClosure(workspace, changed)
        }
    }

    /**
     * @param {Workspace} workspace
     * @param {string[]} names
     * @returns {string[]}
     */
    static reverseClosure(workspace, names) {
        const dependents = new Map(
            workspace.crates.map((crate) => [
                crate.name,
                AffectedCrates.#dependentsOf(workspace, crate.name)
            ])
        )
        return AffectedCrates.#walk(dependents, names)
    }

    /**
     * @param {Workspace} workspace
     * @param {string[]} names
     * @returns {void}
     */
    static check(workspace, names) {
        const differences = names.map((name) =>
            AffectedCrates.differences(
                name,
                AffectedCrates.reverseClosure(workspace, [name]),
                AffectedCrates.invertedNames(
                    AffectedCrates.#run(
                        'cargo',
                        [...AffectedCrates.TREE_ARGUMENTS, name],
                        workspace.root
                    ),
                    workspace
                )
            )
        )
        AffectedCrates.#failOnDifferences(AffectedCrates.#mismatched(differences))
        AffectedCrates.#logChecked(names.length)
    }

    /**
     * @param {string} crate
     * @param {string[]} computed
     * @param {string[]} inverted
     * @returns {CrateDifference}
     */
    static differences(crate, computed, inverted) {
        return {
            crate,
            missing: AffectedCrates.#absent(inverted, computed),
            extra: AffectedCrates.#absent(computed, inverted)
        }
    }

    /**
     * @param {string} output
     * @param {Workspace} workspace
     * @returns {string[]}
     */
    static invertedNames(output, workspace) {
        const names = AffectedCrates.#firstWords(AffectedCrates.#lines(output))
        return AffectedCrates.#unique(AffectedCrates.#workspaceNames(names, workspace))
    }

    /**
     * @param {AffectedResult} result
     * @returns {string}
     */
    static listText(result) {
        return AffectedCrates.#fullOr(
            result,
            AffectedCrates.FULL,
            AffectedCrates.#noneOr(result.crates, result.crates.join('\n'))
        )
    }

    /**
     * @param {AffectedResult} result
     * @returns {string}
     */
    static argumentText(result) {
        const flags = result.crates.flatMap((crate) => [AffectedCrates.PACKAGE_FLAG, crate])
        return AffectedCrates.#fullOr(result, AffectedCrates.WORKSPACE_FLAG, flags.join(' '))
    }

    /**
     * @param {string[]} argv
     * @returns {AffectedOptions}
     */
    static #options(argv) {
        AffectedCrates.#failOnUnknown(AffectedCrates.#unknownFlags(argv))
        return {
            base: AffectedCrates.#valueAfter(argv, '--base', AffectedCrates.DEFAULT_BASE),
            check: argv.includes('--check'),
            args: argv.includes('--args')
        }
    }

    /**
     * @param {string[]} argv
     * @returns {string[]}
     */
    static #unknownFlags(argv) {
        return argv.filter(
            (value) => value.startsWith('-') && !AffectedCrates.KNOWN_FLAGS.includes(value)
        )
    }

    /**
     * @param {string[]} unknown
     * @returns {void}
     */
    static #failOnUnknown(unknown) {
        if (unknown.length > 0) {
            throw new Error(
                `unknown option ${unknown.join(' ')}; use --base <ref>, --check, --args`
            )
        }
    }

    /**
     * @param {string[]} argv
     * @param {string} flag
     * @param {string} fallback
     * @returns {string}
     */
    static #valueAfter(argv, flag, fallback) {
        const index = argv.indexOf(flag)
        return index === -1 ? fallback : argv[index + 1]
    }

    /**
     * @param {CargoMetadata} metadata
     * @returns {MetadataPackage[]}
     */
    static #memberPackages(metadata) {
        return metadata.packages.filter((item) => metadata.workspace_members.includes(item.id))
    }

    /**
     * @param {string} root
     * @param {MetadataPackage} member
     * @param {string[]} names
     * @returns {WorkspaceCrate}
     */
    static #crateFrom(root, member, names) {
        const relative = path.relative(root, path.dirname(member.manifest_path))
        return {
            name: member.name,
            directory: AffectedCrates.#withSlash(relative.split(path.sep).join('/')),
            dependencies: AffectedCrates.#unique(
                AffectedCrates.#workspaceDependencies(member, names)
            )
        }
    }

    /**
     * @param {string} relative
     * @returns {string}
     */
    static #withSlash(relative) {
        return relative === '' ? '' : `${relative}/`
    }

    /**
     * @param {MetadataPackage} member
     * @param {string[]} names
     * @returns {string[]}
     */
    static #workspaceDependencies(member, names) {
        return member.dependencies
            .filter((dependency) => typeof dependency.path === 'string')
            .map((dependency) => dependency.name)
            .filter((name) => names.includes(name))
    }

    /**
     * @param {WorkspaceCrate[]} crates
     * @returns {WorkspaceCrate[]}
     */
    static #deepestFirst(crates) {
        return crates.toSorted((first, second) => second.directory.length - first.directory.length)
    }

    /**
     * @param {Workspace} workspace
     * @param {string} file
     * @returns {ChangedFile}
     */
    static #ownedFile(workspace, file) {
        const owner = workspace.crates.find((crate) => file.startsWith(crate.directory))
        return { file, crate: AffectedCrates.#nameOf(owner) }
    }

    /**
     * @param {WorkspaceCrate | undefined} owner
     * @returns {string}
     */
    static #nameOf(owner) {
        return owner === undefined ? '' : owner.name
    }

    /**
     * @param {ChangedFile[]} owned
     * @returns {string[]}
     */
    static #ownerNames(owned) {
        return owned.filter((item) => item.crate !== '').map((item) => item.crate)
    }

    /**
     * @param {ChangedFile[]} owned
     * @returns {string[]}
     */
    static #looseFiles(owned) {
        return owned.filter((item) => item.crate === '').map((item) => item.file)
    }

    /**
     * @param {string[]} files
     * @returns {string[]}
     */
    static #sharedFiles(files) {
        return files.filter((file) => AffectedCrates.#isShared(file))
    }

    /**
     * @param {string} file
     * @returns {boolean}
     */
    static #isShared(file) {
        return (
            AffectedCrates.FULL_NAMES.includes(path.posix.basename(file)) ||
            AffectedCrates.FULL_FILES.includes(file) ||
            AffectedCrates.#startsWithAny(file, AffectedCrates.FULL_PREFIXES)
        )
    }

    /**
     * @param {string[]} files
     * @returns {string[]}
     */
    static #rustFiles(files) {
        return files.filter((file) => AffectedCrates.#affectsRust(file))
    }

    /**
     * @param {string} file
     * @returns {boolean}
     */
    static #affectsRust(file) {
        return (
            file.endsWith(AffectedCrates.RUST_SUFFIX) ||
            AffectedCrates.RUST_FILES.includes(file) ||
            AffectedCrates.#startsWithAny(file, AffectedCrates.RUST_PREFIXES)
        )
    }

    /**
     * @param {string} file
     * @param {string[]} prefixes
     * @returns {boolean}
     */
    static #startsWithAny(file, prefixes) {
        return prefixes.some((prefix) => file.startsWith(prefix))
    }

    /**
     * @param {Workspace} workspace
     * @param {string} name
     * @returns {string[]}
     */
    static #dependentsOf(workspace, name) {
        return workspace.crates
            .filter((crate) => crate.dependencies.includes(name))
            .map((crate) => crate.name)
    }

    /**
     * @param {Map<string, string[]>} dependents
     * @param {string[]} names
     * @returns {string[]}
     */
    static #walk(dependents, names) {
        let found = AffectedCrates.#unique(names)
        for (let round = 0; round < dependents.size; round += 1) {
            found = AffectedCrates.#expand(dependents, found)
        }
        return found
    }

    /**
     * @param {Map<string, string[]>} dependents
     * @param {string[]} found
     * @returns {string[]}
     */
    static #expand(dependents, found) {
        const next = found.flatMap((name) => AffectedCrates.#dependentsFor(dependents, name))
        return AffectedCrates.#unique([...found, ...next])
    }

    /**
     * @param {Map<string, string[]>} dependents
     * @param {string} name
     * @returns {string[]}
     */
    static #dependentsFor(dependents, name) {
        return dependents.get(name) ?? []
    }

    /**
     * @param {string[]} list
     * @param {string[]} other
     * @returns {string[]}
     */
    static #absent(list, other) {
        return list.filter((name) => !other.includes(name))
    }

    /**
     * @param {CrateDifference[]} differences
     * @returns {CrateDifference[]}
     */
    static #mismatched(differences) {
        return differences.filter(
            (difference) => difference.missing.length + difference.extra.length > 0
        )
    }

    /**
     * @param {CrateDifference[]} mismatched
     * @returns {void}
     */
    static #failOnDifferences(mismatched) {
        if (mismatched.length > 0) {
            throw new Error(
                `the computed list differs from cargo tree --invert: ${JSON.stringify(mismatched)}`
            )
        }
    }

    /**
     * @param {number} count
     * @returns {void}
     */
    static #logChecked(count) {
        if (count > 0) {
            console.error(
                `${AffectedCrates.LOG_PREFIX} --check: ${count} changed crate(s) match cargo tree --invert`
            )
        }
    }

    /**
     * @param {string[]} lines
     * @returns {string[]}
     */
    static #firstWords(lines) {
        return lines.map((line) => line.trim().split(' ')[0])
    }

    /**
     * @param {string[]} names
     * @param {Workspace} workspace
     * @returns {string[]}
     */
    static #workspaceNames(names, workspace) {
        const members = workspace.crates.map((crate) => crate.name)
        return names.filter((name) => members.includes(name))
    }

    /**
     * @param {AffectedResult} result
     * @param {string} whenFull
     * @param {string} otherwise
     * @returns {string}
     */
    static #fullOr(result, whenFull, otherwise) {
        return result.full ? whenFull : otherwise
    }

    /**
     * @param {string[]} crates
     * @param {string} otherwise
     * @returns {string}
     */
    static #noneOr(crates, otherwise) {
        return crates.length === 0 ? AffectedCrates.NONE : otherwise
    }

    /**
     * @param {AffectedOptions} options
     * @param {string} list
     * @param {string} flags
     * @returns {string}
     */
    static #modeText(options, list, flags) {
        return options.args ? flags : list
    }

    /**
     * @param {AffectedOptions} options
     * @param {AffectedResult} result
     * @returns {string[]}
     */
    static #checkedCrates(options, result) {
        return options.check ? result.changed : []
    }

    /**
     * @param {AffectedOptions} options
     * @param {string[]} files
     * @param {AffectedResult} result
     * @returns {void}
     */
    static #logSummary(options, files, result) {
        console.error(
            `${AffectedCrates.LOG_PREFIX} ${files.length} changed file(s) since ${options.base}; changed crates: ${result.changed.join(' ')}`
        )
        for (const reason of result.reasons) {
            console.error(`${AffectedCrates.LOG_PREFIX} full workspace because ${reason} changed`)
        }
    }

    /**
     * @param {string} root
     * @param {string[]} args
     * @returns {string}
     */
    static #git(root, args) {
        return AffectedCrates.#run('git', args, root)
    }

    /**
     * @param {string} command
     * @param {string[]} args
     * @param {string} directory
     * @returns {string}
     */
    static #run(command, args, directory) {
        const result = spawnSync(command, args, {
            cwd: directory,
            encoding: 'utf8',
            env: process.env,
            maxBuffer: AffectedCrates.OUTPUT_LIMIT
        })
        AffectedCrates.#requireSuccess(`${command} ${args.join(' ')}`, result)
        return result.stdout
    }

    /**
     * @param {string} command
     * @param {import('node:child_process').SpawnSyncReturns<string>} result
     * @returns {void}
     */
    static #requireSuccess(command, result) {
        if (result.status !== 0) {
            throw new Error(`${command} failed (exit ${result.status}): ${result.stderr}`)
        }
    }

    /**
     * @param {string} text
     * @returns {string[]}
     */
    static #lines(text) {
        return text.split('\n').filter((line) => line.trim() !== '')
    }

    /**
     * @param {string[]} list
     * @returns {string[]}
     */
    static #unique(list) {
        return [...new Set(list)].sort()
    }
}

if (import.meta.main) {
    AffectedCrates.main(process.argv.slice(2))
}
