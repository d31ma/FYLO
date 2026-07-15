import { VersionRepository } from '../../src/versioning/repository.js'

const [, , root] = process.argv
await new VersionRepository(root).init()
