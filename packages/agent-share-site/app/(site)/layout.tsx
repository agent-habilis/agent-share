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
        // Clay on paper. The accent is the one saturated thing on the page, so
        // it is muted in light and lifted in dark to hold the same weight
        // against each ground.
        color={{
          hue: { light: 16, dark: 18 },
          saturation: { light: 48, dark: 55 },
          lightness: { light: 46, dark: 66 },
        }}
        // Also what the navbar, sidebar and search panel paint on, which is why
        // the warm ground is set here rather than on body in styles.css.
        backgroundColor={{ light: '#faf9f5', dark: '#141312' }}
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
      </body>
    </html>
  )
}
