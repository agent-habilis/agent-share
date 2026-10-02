import nextra from 'nextra'

const withNextra = nextra({})

/** @type {import('next').NextConfig} */
const config = {
  // Built into out/ and served by server.ts, which has no Node to run
  // Next with. trailingSlash writes docs/x/index.html rather than docs/x.html,
  // the shape server.ts already maps `/x/` onto.
  output: 'export',
  trailingSlash: true,
  // No image optimizer in a static export.
  images: { unoptimized: true },
  reactStrictMode: true,
  // In dev the webapp is another server (scripts/dev.ts, under portless), so
  // /app would otherwise hit the export-mode catch-all and die on a runtime
  // error overlay instead of a plain 404. Redirect to the sibling portless
  // host. The build returns nothing: a static export has no redirects.
  async redirects() {
    const own = process.env.PORTLESS_URL
    if (process.env.NODE_ENV !== 'development' || !own) return []
    const app = own.replace('.agent-share-docs.', '.agent-share.')
    if (app === own) return []
    return [{ source: '/app/:path*', destination: `${app}/app/:path*`, permanent: false }]
  },
}

export default withNextra(config)
