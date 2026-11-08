import { describe, expect, test } from 'bun:test'
import DocsSample from '../../website/client/components/docs/sample/tac.js'
import Showcase from '../../website/client/components/code/showcase/tac.js'
import DocsContent from '../../website/client/components/docs/content/tac.js'

function sampleCode(topic, lang) {
    const sample = new DocsSample({ topic })
    sample.$lang = lang
    return sample.code()
}

function showcaseCode(tab, lang) {
    const showcase = new Showcase()
    showcase.$tab = tab
    showcase.$lang = lang
    return showcase.currentCode()
}

function contentCode(section, lang) {
    const content = new DocsContent()
    content.$section = section
    content.$lang = lang
    return content.code()
}

describe('website language samples match the shipped clients', () => {
    test('Go and Rust pass the collection kind their create methods require', () => {
        expect(sampleCode('crud', 'go')).toContain('db.Collection("users").Create("document")')
        expect(sampleCode('crud', 'rust')).toContain('db.collection("users").create("document")?;')
    })

    test('typed clients read the put id as a string', () => {
        expect(sampleCode('crud', 'go')).toContain('id := idValue.(string)')
        expect(sampleCode('crud', 'csharp')).toContain('.GetString();')
        expect(sampleCode('crud', 'swift')).toContain('as! String')
        expect(sampleCode('crud', 'kotlin')).toContain('as String')
        expect(sampleCode('crud', 'rust')).toContain('reply["result"].as_str()')
        expect(sampleCode('crud', 'rust')).toContain('.get(&id)?;')
        expect(sampleCode('crud', 'java')).toContain('.readTree(response).get("result")')
    })

    test('Go samples import fmt and use every variable they declare', () => {
        const query = sampleCode('query', 'go')
        expect(query).toContain('import "fmt"')
        expect(query).toContain('fmt.Println(admins, rows)')
        expect(sampleCode('find-ops', 'go')).toContain(
            'fmt.Println(exact, adults, engineers, privileged)'
        )
        expect(showcaseCode('sql', 'go')).toContain('import "fmt"')
        expect(contentCode('sql', 'go')).toContain('import "fmt"')
    })

    test('browser samples resolve cursors and use the escaping sql tag', () => {
        const crud = sampleCode('crud', 'web')
        expect(crud).toContain('await db.users.get(id).once()')
        const query = sampleCode('query', 'web')
        expect(query).toContain('await Array.fromAsync(db.users.find(')
        expect(query).toContain('.collect())')
        expect(query).toContain("await db.sql`SELECT * FROM users WHERE role = 'admin'`")
        expect(query).not.toContain('executeSQL')
    })

    test('Swift samples add Fylo.swift to the app target instead of importing a module', () => {
        for (const code of [
            sampleCode('crud', 'swift'),
            sampleCode('configuration', 'swift'),
            showcaseCode('start', 'swift'),
            contentCode('install', 'swift'),
            contentCode('crud', 'swift')
        ]) {
            expect(code).not.toContain('import Fylo')
        }
    })

    test('raw-file bucket samples are shown only for binary-backed clients', () => {
        for (const lang of ['web', 'swift', 'kotlin', 'flutter']) {
            const code = sampleCode('bucket', lang)
            expect(code).toContain('need the native fylo binary')
            expect(code).not.toContain('create(')
        }
        expect(sampleCode('bucket', 'node')).toContain("await db.assets.create('file')")
    })

    test('local-only samples create the collection before the first put', () => {
        for (const code of [
            showcaseCode('start', 'web'),
            contentCode('install', 'web'),
            contentCode('install', 'swift'),
            contentCode('install', 'kotlin'),
            contentCode('install', 'flutter')
        ]) {
            expect(code.indexOf('create()')).toBeGreaterThan(-1)
            expect(code.indexOf('create()')).toBeLessThan(code.indexOf('put('))
        }
    })
})
