export function resolveSite(config, siteName) {
    const site = config.sites?.[siteName]
    if (!site) throw new Error(`Unknown site ${JSON.stringify(siteName)}`)
    if (site.extends === undefined) return site
    return { ...baseSite(config, siteName, site.extends), ...withoutExtends(site) }
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
