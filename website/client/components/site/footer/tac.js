export default class extends Tac {
  /** @type {string} */
  @env('PUBLIC_SITE_URL', 'https://fylo.del.ma')
  siteUrl

  linkGroups = [
    {
      title: 'Resources',
      links: [
        { label: 'Documentation', href: '/docs' },
        { label: 'Download', href: '/download' },
        { label: 'Source code', href: 'https://github.com/d31ma/Fylo' },
        { label: 'Releases', href: 'https://github.com/d31ma/Fylo/releases' },
      ],
    },
    {
      title: 'Ecosystem',
      links: [
        { label: 'Language clients', href: 'https://github.com/d31ma/Fylo/tree/main/clients' },
        { label: 'Tachyon', href: 'https://github.com/d31ma/Tachyon' },
        { label: 'TTID', href: 'https://github.com/d31ma/ttid' },
        { label: 'CHEX', href: 'https://github.com/d31ma/chex' },
      ],
    },
    {
      title: 'Legal',
      links: [{ label: 'MIT License', href: 'https://github.com/d31ma/Fylo/blob/main/LICENSE' }],
    },
  ]
}
