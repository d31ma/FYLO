import fs from 'node:fs'
import path from 'node:path'

const suiteRoot = process.argv[2] ?? '.blackbox-tests'
const fyloRoot = path.join(suiteRoot, 'fylo')

/**
 * @callback SourcePatcher
 * @param {string} source
 * @param {string} filePath
 * @returns {string}
 */

/**
 * Reads, patches, and writes a FYLO NightJar contract file.
 *
 * @param {string} relativePath
 * @param {SourcePatcher} patcher
 * @returns {void}
 */
function patchFile(relativePath, patcher) {
    const filePath = path.join(fyloRoot, relativePath)
    let source = fs.readFileSync(filePath, 'utf8')
    source = patcher(source, filePath)
    fs.writeFileSync(filePath, source)
}

/**
 * Replaces required fixture text and fails loudly if NightJar changes shape.
 *
 * @param {string} source
 * @param {string} search
 * @param {string} replacement
 * @param {string} filePath
 * @returns {string}
 */
function replaceRequired(source, search, replacement, filePath) {
    if (!source.includes(search)) {
        throw new Error(`Unexpected NightJar FYLO fixture format in ${filePath}`)
    }
    return source.replace(search, replacement)
}

/**
 * Configures generated consumers to install FYLO's public dependencies from npm.
 *
 * @returns {void}
 */
function patchHelper() {
    const helperPath = path.join(fyloRoot, 'helpers.mjs')
    let source = fs.readFileSync(helperPath, 'utf8')
    const marker = "  const install = run('bun', ['add', tarball], { cwd: root })\n"
    const injected =
        '  await writeFile(\n' +
        "    path.join(root, '.npmrc'),\n" +
        "    '@d31ma:registry=https://registry.npmjs.org/\\n'\n" +
        '  )\n\n' +
        marker
    source = replaceRequired(source, marker, injected, helperPath)
    fs.writeFileSync(helperPath, source)
}

/**
 * Updates public package contract tests for FYLO's RLS-backed auth model.
 *
 * @returns {void}
 */
function patchPackageContract() {
    patchFile('package-contract.test.mjs', (initialSource, filePath) => {
        let source = initialSource.replaceAll('@delma/fylo', '@d31ma/fylo')
        const oldAuthTest = `test('auth policy wrapper fails closed and gates scoped operations', async () => {
  await consumer.runModule(\`
    import { mkdtemp, rm } from 'node:fs/promises'
    import os from 'node:os'
    import path from 'node:path'
    import Fylo, { FyloAuthError } from '@d31ma/fylo'

    const root = await mkdtemp(path.join(os.tmpdir(), '\${uniqueName('fylo-auth')}-'))
    try {
      const openFylo = new Fylo({ root })
      let missingPolicy = false
      try {
        openFylo.as({ subjectId: 'user-1' })
      } catch (err) {
        missingPolicy = err.message.includes('auth policy is not configured')
      }
      if (!missingPolicy) throw new Error('as() should fail closed without a policy')

      const calls = []
      const fylo = new Fylo({
        root,
        auth: {
          authorize(input) {
            calls.push(input)
            return input.auth.subjectId === 'user-1' && input.action !== 'doc:delete'
          },
        },
      })
      const db = fylo.as({ subjectId: 'user-1', tenantId: 'tenant-a', roles: ['writer'] })
      const collection = 'blackbox-auth'

      await db.createCollection(collection)
      const id = await db.putData(collection, { tenantId: 'tenant-a', title: 'Owned' })
      const doc = await db.getDoc(collection, id).once()
      if (doc[id].title !== 'Owned') throw new Error('authorized read failed')

      let denied = false
      try {
        await db.delDoc(collection, id)
      } catch (err) {
        denied = err instanceof FyloAuthError
      }
      if (!denied) throw new Error('delete should be denied by policy')
      if (!calls.some((call) => call.action === 'doc:delete')) throw new Error('delete was not authorized')
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  \`)
})
`
        const newAuthTest = `test('auth policy wrapper fails closed and gates scoped operations', async () => {
  await consumer.runModule(\`
    import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
    import os from 'node:os'
    import path from 'node:path'
    import Fylo, { FyloAuthError } from '@d31ma/fylo'

    const root = await mkdtemp(path.join(os.tmpdir(), '\${uniqueName('fylo-auth')}-'))
    const schemaDir = await mkdtemp(path.join(os.tmpdir(), '\${uniqueName('fylo-auth-schema')}-'))
    const previousSchema = process.env.FYLO_SCHEMA
    process.env.FYLO_SCHEMA = schemaDir
    try {
      const openFylo = new Fylo({ root })
      let missingPolicy = false
      try {
        openFylo.as({ subjectId: 'user-1' })
      } catch (err) {
        missingPolicy = err.message.includes('FYLO RLS is not enabled')
      }
      if (!missingPolicy) throw new Error('as() should fail closed without RLS')

      const collection = 'blackbox-auth'
      await mkdir(path.join(schemaDir, collection), { recursive: true })
      await writeFile(path.join(schemaDir, collection, 'rules.json'), JSON.stringify({
        version: 1,
        roles: [{
          name: 'writer',
          apply_when: { $eq: ['%%user.subjectId', 'user-1'] },
          allow_actions: ['collection:create'],
          read: { filter: {} },
          insert: { predicate: true },
          update: { filter: {} },
        }],
      }))

      const fylo = new Fylo({ root, rls: true })
      const db = fylo.as({ subjectId: 'user-1', tenantId: 'tenant-a', roles: ['writer'] })

      await db.createCollection(collection)
      const id = await db.putData(collection, { tenantId: 'tenant-a', title: 'Owned' })
      const doc = await db.getDoc(collection, id).once()
      if (doc[id].title !== 'Owned') throw new Error('authorized read failed')

      let denied = false
      try {
        await db.delDoc(collection, id)
      } catch (err) {
        denied = err instanceof FyloAuthError
      }
      if (!denied) throw new Error('delete should be denied by rules')
    } finally {
      if (previousSchema === undefined) delete process.env.FYLO_SCHEMA
      else process.env.FYLO_SCHEMA = previousSchema
      await rm(root, { recursive: true, force: true })
      await rm(schemaDir, { recursive: true, force: true })
    }
  \`)
})
`
        return replaceRequired(source, oldAuthTest, newAuthTest, filePath)
    })
}

/**
 * Updates security contracts for versioned schema manifests and prefix indexes.
 *
 * @returns {void}
 */
function patchSecurityContract() {
    patchFile('security-contract.test.mjs', (initialSource, filePath) => {
        let source = initialSource.replaceAll('@delma/fylo', '@d31ma/fylo')
        source = source.replace(
            "    import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'",
            "    import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises'"
        )
        source = source.replace(
            "    import { mkdtemp, rm, writeFile } from 'node:fs/promises'",
            "    import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'"
        )
        source = source
            .replaceAll('process.env.SCHEMA_DIR', 'process.env.FYLO_SCHEMA')
            .replaceAll('process.env.ENCRYPTION_KEY', 'process.env.FYLO_ENCRYPTION_KEY')
            .replaceAll('process.env.CIPHER_SALT', 'process.env.FYLO_CIPHER_SALT')
        source = replaceRequired(
            source,
            "    const collection = 'blackbox-secrets'\n",
            `    const collection = 'blackbox-secrets'

    async function readTree(target) {
      let text = ''
      for (const entry of await readdir(target, { withFileTypes: true })) {
        const child = path.join(target, entry.name)
        text += entry.isDirectory()
          ? await readTree(child)
          : child + '\\\\n' + await readFile(child, 'utf8') + '\\\\n'
      }
      return text
    }
`,
            filePath
        )
        source = replaceRequired(
            source,
            `      await writeFile(path.join(schemaDir, collection + '.json'), JSON.stringify({
        $encrypted: ['email', 'profile/ssn'],
      }))
`,
            `      await mkdir(path.join(schemaDir, collection, 'history'), { recursive: true })
      await writeFile(path.join(schemaDir, collection, 'history', 'v1.schema.json'), JSON.stringify({
        $encrypted: ['email', 'profile/ssn'],
      }))
      await writeFile(path.join(schemaDir, collection, 'manifest.json'), JSON.stringify({
        current: 'v1',
        versions: [{ v: 'v1', addedAt: '2026-04-28T00:00:00.000Z' }],
      }))
`,
            filePath
        )
        source = replaceRequired(
            source,
            `      const indexFile = await readFile(
        path.join(root, '.collections', collection, 'indexes', collection + '.idx.json'),
        'utf8'
      )
`,
            "      const indexFile = await readTree(path.join(root, '.collections', collection, 'index'))\n",
            filePath
        )
        source = replaceRequired(
            source,
            `      await writeFile(
        path.join(root, '.collections', collection, 'indexes', collection + '.idx.json'),
        '{not-json',
        'utf8'
      )

      const iterator = fylo.findDocs(collection, {
        $ops: [{ title: { $eq: 'anything' } }],
      }).collect()

      let badIndex = false
      try {
        await iterator.next()
      } catch (err) {
        badIndex = err.message === 'Invalid FYLO index file for collection: ' + collection
      }
      if (!badIndex) throw new Error('corrupted index did not return sanitized error')
`,
            `      const staleIndexDir = path.join(root, '.collections', collection, 'index')
      await mkdir(staleIndexDir, { recursive: true })
      await writeFile(path.join(staleIndexDir, 'leftover.tmp'), 'not-a-catalog-file', 'utf8')

      const rows = []
      for await (const doc of fylo.findDocs(collection, {
        $ops: [{ title: { $eq: 'anything' } }],
      }).collect()) {
        rows.push(doc)
      }
      if (rows.length !== 0) throw new Error('stale local index file was not ignored')
`,
            filePath
        )
        return source
    })
}

patchHelper()
patchPackageContract()
patchSecurityContract()
