import type { Metadata } from 'next'
import type { ReactNode } from 'react'
import { Footer, Layout, Navbar } from 'nextra-theme-docs'
import { Head } from 'nextra/components'
import { getPageMap } from 'nextra/page-map'

import 'nextra-theme-docs/style.css'
import './styles.css'

const description =
  'Share a folder with peers, or mount a peer\'s folder locally. Read-only, lazy, no daemon.'

export const metadata: Metadata = {
  metadataBase: new URL('https://agent-share.dev'),
  title: { default: 'agent-share', template: '%s — agent-share' },
  description,
  openGraph: {
    type: 'website',
    title: 'agent-share 🗄️',
    description,
  },
}

const FOOTER_LINKS = [
  ['GitHub', 'https://github.com/agent-habilis/agent-share'],
  ['Docs', '/docs'],
  ['License', 'https://github.com/agent-habilis/agent-share/blob/main/LICENSE'],
  ['agent-habilis', 'https://agent-habilis.com'],
  ['iroh', 'https://www.iroh.computer/'],
] as const

export default async function SiteLayout({ children }: { children: ReactNode }) {
  const pageMap = await getPageMap()
  return (
    <html lang="en" dir="ltr" suppressHydrationWarning>
      <Head
        faviconGlyph="🗄️"
        color={{
          hue: { light: 213, dark: 210 },
          saturation: { light: 86, dark: 94 },
          lightness: { light: 42, dark: 67 },
        }}
      />
      <body>
        <Layout
          pageMap={pageMap}
          docsRepositoryBase="https://github.com/agent-habilis/agent-share/tree/main/packages/agent-share-site"
          sidebar={{ defaultMenuCollapseLevel: 1 }}
          // The site follows the OS color scheme; there is no switch to pick one.
          darkMode={false}
          navbar={
            <Navbar
              logo={<b>agent-share 🗄️</b>}
              projectLink="https://github.com/agent-habilis/agent-share"
            />
          }
          footer={
            <Footer>
              <ul className="foot-links">
                {FOOTER_LINKS.map(([label, href]) => (
                  <li key={href}>
                    <a href={href} {...(href.startsWith('/') ? {} : { target: '_blank', rel: 'noopener' })}>
                      {label}
                    </a>
                  </li>
                ))}
              </ul>
            </Footer>
          }
        >
          {children}
        </Layout>
        {/*
          The webapp opens in its own tab. Every link in the content says so in
          its own markup; the navbar's cannot, because it comes from
          `content/_meta.ts`, and a page item there carries a title and an href
          and nothing else. Hence this, rather than rebuilding the nav item by
          hand to hang two attributes off it. Without JavaScript the link still
          works — it opens in the same tab.
        */}
        <script
          dangerouslySetInnerHTML={{
            __html: `document.querySelectorAll('a[href^="/app"]').forEach(function(a){a.target="_blank";a.rel="noopener"})`,
          }}
        />
      </body>
    </html>
  )
}
