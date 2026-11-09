export function resolveSite(config, siteName) {
    const site = config.sites?.[siteName]
    if (!site) throw new Error(`Unknown site ${JSON.stringify(siteName)}`)
    return site
}
