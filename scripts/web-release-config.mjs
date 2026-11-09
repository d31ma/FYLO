export function resolveSite(config, siteName) {
    const site = config.sites?.[siteName]
    if (!site) throw new Error(`Unknown site ${JSON.stringify(siteName)}`)
    if (site.extends === undefined) return site
    return { ...baseSite(config, siteName, site.extends), ...withoutExtends(site) }
}

export async function withBranchAuthorization(
    site,
    { readBranch = readAmplifyBranch, mask = maskInActions } = {}
) {
    if (site.basicAuthFromBranch !== true) return site
    const branch = await readBranch(site.appId, site.branch)
    return { ...site, authorization: branchAuthorization(branch, mask) }
}

function baseSite(config, siteName, baseName) {
    const base = config.sites?.[baseName]
    if (!base || base.extends !== undefined) {
        throw new Error(`Site ${siteName} must extend a defined site that extends no other`)
    }
    return base
}

function withoutExtends(site) {
    const { extends: _base, ...override } = site
    return override
}

function branchAuthorization(branch, mask) {
    if (!branch.enableBasicAuth) return undefined
    return `Basic ${maskedCredentials(branch.basicAuthCredentials, mask)}`
}

function maskedCredentials(credentials, mask) {
    if (!credentials) {
        throw new Error(
            'Basic auth is on for the branch, but GetBranch returned no basicAuthCredentials'
        )
    }
    mask(credentials)
    return credentials
}

async function readAmplifyBranch(appId, branchName) {
    const child = Bun.spawn(
        [
            'aws',
            'amplify',
            'get-branch',
            '--app-id',
            appId,
            '--branch-name',
            branchName,
            '--output',
            'json'
        ],
        { stdout: 'pipe', stderr: 'pipe' }
    )
    const [status, stdout, stderr] = await Promise.all([
        child.exited,
        new Response(child.stdout).text(),
        new Response(child.stderr).text()
    ])
    if (status !== 0) {
        throw new Error(`aws amplify get-branch failed: ${stderr.trim() || `status ${status}`}`)
    }
    return JSON.parse(stdout).branch
}

function maskInActions(secret) {
    if (process.env.GITHUB_ACTIONS === 'true') console.log(`::add-mask::${secret}`)
}
